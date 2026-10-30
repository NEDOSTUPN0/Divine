//! The download centre: an indicator tucked into the bottom of the sidebar with
//! a popover listing every job, each with its own pause and cancel.
//!
//! Nautilus lays its file operations out the same way (a flat menu button in the
//! sidebar's bottom bar, one row per operation with a progress bar, a detail
//! line and a stop button), which is where the shape of this comes from.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::legendary::{Control, InstallEnd, InstallMode, Place, Progress};
use crate::models::{human_size, Game};
use crate::ui::{accessible_label, spawn_bg, window::App};
use crate::umu;

/// What a job is doing. Both sit on the same list, but only a download can be
/// paused, written down for later, or picked up again after a restart: a
/// half-finished copy is worth nothing, so a move that stops is a move undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Download(InstallMode),
    Move,
}

impl Kind {
    fn mode(self) -> Option<InstallMode> {
        match self {
            Kind::Download(mode) => Some(mode),
            Kind::Move => None,
        }
    }

    fn resumable(self) -> bool {
        matches!(self, Kind::Download(mode) if mode.resumable())
    }

    fn verb(self) -> &'static str {
        match self {
            Kind::Download(mode) => mode.verb(),
            Kind::Move => "Moving",
        }
    }
}

/// One download, alive from the first click until it finishes or is cancelled.
/// A paused job keeps its row: legendary left a resume file behind, so starting
/// it again continues rather than restarts.
struct Job {
    app_name: String,
    title: String,
    base_path: Option<String>,
    /// The folder a first install goes into, settled before its first chunk
    /// and kept from then on: legendary works the path out afresh on every
    /// resume, and a different answer the second time would start the
    /// download over somewhere else.
    folder: RefCell<Option<String>>,
    kind: Kind,
    /// Manifest tags to install, empty for the game's default set.
    tags: RefCell<Vec<String>>,
    /// Set on a job picked up from legendary alone, where what it was fetching
    /// has to be worked out before it can carry on.
    unknown_tags: Cell<bool>,
    /// "Installing", "Updating" or "Repairing": every label about this job.
    what: &'static str,
    /// Replaced on every run, since each run has its own process.
    control: RefCell<Arc<Control>>,
    paused: Cell<bool>,
    cancelled: Cell<bool>,
    /// Set when the cancel was meant to take the half-finished files with it.
    discard: Cell<bool>,
    fraction: Cell<f64>,
    row: gtk::ListBoxRow,
    status: gtk::Label,
    bar: gtk::ProgressBar,
    details: gtk::Label,
    pause: gtk::Button,
    cancel: gtk::Button,
}

/// Something Divine is waiting on rather than running: a Proton build coming
/// down, either because the Proton page asked for it or because umu decided a
/// launch needed a newer one.
///
/// It shows beside the downloads because that is where somebody looks when the
/// launcher seems to be doing nothing — a game that takes a minute longer than
/// usual to start is a gigabyte of Proton arriving. There is nothing to pause
/// or cancel: the launch is already waiting on it.
struct Task {
    title: String,
    fraction: Cell<f64>,
    row: gtk::ListBoxRow,
    status: gtk::Label,
    bar: gtk::ProgressBar,
}

pub struct Downloads {
    /// The sidebar's bottom bar. Hidden while nothing is downloading.
    pub bar: gtk::Box,
    summary: gtk::Label,
    /// The ring in front of the label, so a glance at the closed bar is enough.
    ring: gtk::DrawingArea,
    progress: Rc<Cell<f64>>,
    list: gtk::ListBox,
    jobs: RefCell<HashMap<String, Rc<Job>>>,
    tasks: RefCell<HashMap<String, Rc<Task>>>,
}

impl Downloads {
    pub fn new() -> Self {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("boxed-list");

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(320)
            .propagate_natural_height(true)
            .child(&list)
            .build();

        // Fixed width, not just a maximum: the label's text changes with every
        // percent, and a label that resizes drags the button and its popover
        // along with it.
        let summary = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(20)
            .max_width_chars(20)
            .xalign(0.0)
            .hexpand(true)
            .build();

        let progress = Rc::new(Cell::new(0.0));
        let ring = progress_ring(&progress);

        let content = gtk::Box::builder().spacing(9).build();
        content.append(&ring);
        content.append(&summary);

        let button = gtk::MenuButton::builder()
            .popover(&gtk::Popover::builder().child(&scroller).build())
            .child(&content)
            .tooltip_text("Show Downloads")
            .build();
        accessible_label(&button, "Show Downloads");
        button.add_css_class("flat");

        let bar = gtk::Box::builder().visible(false).build();
        bar.add_css_class("toolbar");
        bar.append(&button);

        Self {
            bar,
            summary,
            ring,
            progress,
            list,
            jobs: RefCell::new(HashMap::new()),
            tasks: RefCell::new(HashMap::new()),
        }
    }

    /// Puts a task on the list, or moves the one already there along.
    ///
    /// `fraction` is `None` for work that cannot say how far along it is, which
    /// is most of it: umu reports what it is doing, never how much is left.
    pub fn show_task(&self, id: &str, title: &str, status: &str, fraction: Option<f64>) {
        let task = self.tasks.borrow().get(id).cloned();
        let task = match task {
            Some(task) => task,
            None => {
                let task = Rc::new(task_row(title));
                self.list.append(&task.row);
                self.tasks.borrow_mut().insert(id.to_string(), task.clone());
                task
            }
        };

        task.status.set_label(status);
        match fraction {
            Some(fraction) => {
                task.fraction.set(fraction);
                task.bar.set_fraction(fraction);
            }
            None => {
                task.bar.pulse();
                // A pulsing bar has no fraction to hand the ring, and calling
                // it half done would be a guess. It counts as nothing.
                task.fraction.set(0.0);
            }
        }
        self.refresh_indicator();
    }

    /// Moves the pulse of a task with no fraction along one step. False once
    /// the task is gone, which is how the timer that calls this stops.
    pub fn pulse(&self, id: &str) -> bool {
        let Some(task) = self.tasks.borrow().get(id).cloned() else {
            return false;
        };
        if task.fraction.get() == 0.0 {
            task.bar.pulse();
        }
        true
    }

    /// Takes the task off the list. Unknown ids are ignored, so a caller can
    /// finish twice without checking.
    pub fn finish_task(&self, id: &str) {
        let Some(task) = self.tasks.borrow_mut().remove(id) else {
            return;
        };
        self.list.remove(&task.row);
        self.refresh_indicator();
    }

    /// The bar only says as much as fits: one job by name, several by count.
    /// The ring carries the same number for anyone not reading the label.
    fn refresh_indicator(&self) {
        let jobs = self.jobs.borrow();
        let tasks = self.tasks.borrow();
        let running = jobs.len() + tasks.len();
        self.bar.set_visible(running > 0);

        let done: f64 = jobs.values().map(|job| job.fraction.get()).sum::<f64>()
            + tasks.values().map(|task| task.fraction.get()).sum::<f64>();
        self.progress.set(match running {
            0 => 0.0,
            running => done / running as f64,
        });
        self.ring.queue_draw();

        let label = match (jobs.len(), tasks.len()) {
            (0, 0) => String::new(),
            (1, 0) => {
                let job = jobs.values().next().expect("checked above");
                if job.paused.get() {
                    format!("{} · paused", job.title)
                } else {
                    format!("{} · {:.0}%", job.title, job.fraction.get() * 100.0)
                }
            }
            (0, 1) => {
                let task = tasks.values().next().expect("checked above");
                task.title.clone()
            }
            // "Downloads" would be a lie with a move among them.
            (n, 0) if jobs.values().all(|job| job.kind != Kind::Move) => format!("{n} downloads"),
            _ => format!("{running} in progress"),
        };
        self.summary.set_label(&label);
    }
}

impl Downloads {
    /// What is on the list right now, as titles, and whether any of it would
    /// have to be started over rather than picked up again.
    pub fn in_flight(&self) -> (Vec<String>, bool) {
        let jobs = self.jobs.borrow();
        let titles = jobs.values().map(|job| job.title.clone()).collect();
        let restartable = jobs.values().any(|job| !job.kind.resumable());
        (titles, restartable)
    }

    /// Downloads are children of this process, so they are stopped rather than
    /// orphaned when it exits. legendary's resume file survives, and the list
    /// written to disk brings the jobs back paused.
    pub fn stop_all(&self) {
        for job in self.jobs.borrow().values() {
            job.control.borrow().request_stop();
        }
    }
}

impl Default for Downloads {
    fn default() -> Self {
        Self::new()
    }
}

/// One task's row: what it is, what it is doing, and a bar that pulses when
/// there is no telling how far along it is.
fn task_row(title: &str) -> Task {
    let name = gtk::Label::builder().label(title).xalign(0.0).build();
    name.add_css_class("heading");

    let status = gtk::Label::builder().xalign(0.0).build();
    status.add_css_class("dim-label");
    status.add_css_class("caption");

    let bar = gtk::ProgressBar::builder().pulse_step(0.1).build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(9)
        .margin_bottom(9)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&name);
    content.append(&bar);
    content.append(&status);

    let row = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(&content)
        .build();

    Task {
        title: title.to_string(),
        fraction: Cell::new(0.0),
        row,
        status,
        bar,
    }
}

/// The indicator Nautilus uses for file operations: an arc of the widget's own
/// colour for what is done, the rest of the circle in a quarter of it. Drawn
/// rather than themed, since no icon can carry a fraction.
fn progress_ring(progress: &Rc<Cell<f64>>) -> gtk::DrawingArea {
    let ring = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .valign(gtk::Align::Center)
        .build();

    let value = progress.clone();
    ring.set_draw_func(move |area, cr, width, height| {
        let colour = area.color();
        let radius = (width.min(height) as f64) / 2.0 - 1.5;
        let done = value.get().clamp(0.0, 1.0);
        // Twelve o'clock is where a clock starts, and so does this.
        let start = -std::f64::consts::FRAC_PI_2;
        let end = start + done * std::f64::consts::PI * 2.0;

        cr.translate(width as f64 / 2.0, height as f64 / 2.0);
        cr.set_line_width(2.0);

        cr.set_source_rgba(
            colour.red() as f64,
            colour.green() as f64,
            colour.blue() as f64,
            colour.alpha() as f64 * 0.25,
        );
        cr.arc(0.0, 0.0, radius, end, start + std::f64::consts::PI * 2.0);
        let _ = cr.stroke();

        cr.set_source_rgba(
            colour.red() as f64,
            colour.green() as f64,
            colour.blue() as f64,
            colour.alpha() as f64,
        );
        cr.arc(0.0, 0.0, radius, start, end);
        let _ = cr.stroke();
    });
    ring
}

/// Shows what umu is doing about Proton while a game is starting.
///
/// umu fetches Proton in the middle of a launch, and from the outside that is a
/// game taking a minute longer than usual for no visible reason. The only word
/// of it is on the launch's own output, so this listens there and puts a row in
/// the download centre for as long as it lasts.
pub fn watch_proton(app: &Rc<App>) {
    let (tx, rx) = async_channel::unbounded::<umu::Fetching>();
    crate::log::listen(move |record| {
        // umu is the one fetching; legendary and the game have nothing to say
        // about it, and neither does anything Divine downloads itself.
        if record.source != crate::log::Source::Tool("umu") {
            return true;
        }
        match umu::fetching(&record.message) {
            Some(step) => tx.send_blocking(step).is_ok(),
            None => true,
        }
    });

    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        let mut pulsing = false;
        while let Ok(step) = rx.recv().await {
            let Some(app) = weak.upgrade() else {
                break;
            };
            // One timer for the whole fetch, started when the row appears: umu
            // says nothing at all between one stage and the next, and a bar
            // that has stopped moving reads as work that has stopped.
            if step != umu::Fetching::Ready && !pulsing {
                pulse_task(&app, PROTON_TASK);
                pulsing = true;
            }
            if step == umu::Fetching::Ready {
                pulsing = false;
            }
            match step {
                umu::Fetching::Downloading(what) => app.downloads.show_task(
                    PROTON_TASK,
                    "Proton",
                    &format!("Downloading {what}"),
                    None,
                ),
                umu::Fetching::Patching => {
                    app.downloads
                        .show_task(PROTON_TASK, "Proton", "Applying an update", None)
                }
                umu::Fetching::Extracting(what) => app.downloads.show_task(
                    PROTON_TASK,
                    "Proton",
                    &format!("Unpacking {what}"),
                    None,
                ),
                umu::Fetching::Ready => app.downloads.finish_task(PROTON_TASK),
            }
        }
    });
}

/// The one row umu's fetching ever needs: a launch waits on one build.
const PROTON_TASK: &str = "proton-umu";

/// Keeps a pulsing task moving. A bar that has stopped reads as work that has
/// stopped, and umu says nothing at all between "Downloading" and "Extracting".
pub fn pulse_task(app: &Rc<App>, id: &'static str) {
    let weak = Rc::downgrade(app);
    glib::timeout_add_local(Duration::from_millis(120), move || {
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        match app.downloads.pulse(id) {
            true => glib::ControlFlow::Continue,
            false => glib::ControlFlow::Break,
        }
    });
}

/// Queues an install, update or repair and shows it in the download centre.
pub fn start(
    app: &Rc<App>,
    game: &Game,
    base_path: Option<String>,
    mode: InstallMode,
    tags: Vec<String>,
) {
    if app.busy.borrow().contains_key(&game.app_name) {
        app.toast("That game is already busy");
        return;
    }

    let what = mode.verb();
    let job = Rc::new(Job::new(
        &game.app_name,
        &game.title,
        base_path,
        Kind::Download(mode),
        tags,
    ));
    attach(app, &job);

    // What cannot be picked up later is not offered as a pause; see
    // InstallMode::resumable for why, and what to do about it upstream.
    if !mode.resumable() {
        job.pause.set_visible(false);
        job.row
            .set_tooltip_text(Some("Components cannot be paused, only stopped"));
    }

    app.toast(&format!("{what} {}", game.title));
    run(app, &job);
}

/// Puts a job on the list and wires its two buttons. Shared by a download that
/// is starting now and one picked back up from a previous run.
fn attach(app: &Rc<App>, job: &Rc<Job>) {
    app.downloads.list.append(&job.row);
    app.downloads
        .jobs
        .borrow_mut()
        .insert(job.app_name.clone(), job.clone());
    // A copy cannot be picked up halfway, so a move has nothing to pause.
    job.pause.set_visible(job.kind != Kind::Move);

    let weak = Rc::downgrade(app);
    let target = job.clone();
    job.pause.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            toggle_pause(&app, &target);
        }
    });

    let weak = Rc::downgrade(app);
    let target = job.clone();
    job.cancel.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            cancel(&app, &target);
        }
    });
    remember(app);
}

/// Downloads outlive the launcher: legendary can carry on where it stopped, so
/// what was on the list is written down and offered again on the next start.
/// Nothing resumes by itself — the button is there, the choice stays with the
/// person who paused it.
#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    app_name: String,
    title: String,
    base_path: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    mode: InstallMode,
    #[serde(default)]
    tags: Vec<String>,
}

fn state_file() -> std::path::PathBuf {
    crate::config::config_dir().join("downloads.json")
}

fn remember(app: &Rc<App>) {
    let saved: Vec<Saved> = app
        .downloads
        .jobs
        .borrow()
        .values()
        // A move has nothing to come back to: the copy either finished or the
        // game never left where it was.
        .filter_map(|job| {
            Some(Saved {
                app_name: job.app_name.clone(),
                title: job.title.clone(),
                base_path: job.base_path.clone(),
                folder: job.folder.borrow().clone(),
                mode: job.kind.mode()?,
                tags: job.tags.borrow().clone(),
            })
        })
        .collect();
    let path = state_file();
    if saved.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Ok(text) = serde_json::to_string(&saved) {
        let _ = std::fs::write(path, text);
    }
}

/// Brings back what was left unfinished, paused and ready to carry on.
pub fn restore(app: &Rc<App>) {
    // Nothing written down is not the same as nothing to resume: legendary's
    // own files are checked either way.
    let saved: Vec<Saved> = std::fs::read_to_string(state_file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();

    let lgd = app.lgd.borrow().clone();
    let installed: Vec<String> = lgd
        .list_installed()
        .unwrap_or_default()
        .into_iter()
        .map(|game| game.app_name)
        .collect();

    for entry in saved {
        // Without a resume file there is nothing to carry on from, and the
        // download either finished or never got as far as a single chunk.
        if !lgd.can_resume(&entry.app_name) {
            continue;
        }
        // Nothing that cannot be picked up is offered as if it could be.
        if !entry.mode.resumable() || installed.contains(&entry.app_name) {
            lgd.forget_resume(&entry.app_name);
            continue;
        }
        restore_job(
            app,
            &entry.app_name,
            &entry.title,
            entry.base_path,
            entry.folder,
            entry.mode,
            entry.tags,
        );
    }

    // legendary's own resume files outlast anything Divine wrote down, so what
    // is left over is offered too. Which components such a job was after is
    // not known yet; that is worked out from the manifest when it is resumed,
    // and needs the network, so it waits until somebody asks for it.
    for app_name in lgd.resumable() {
        if app.downloads.jobs.borrow().contains_key(&app_name) {
            continue;
        }
        // A game already on disk was having something added to it, and that is
        // the case legendary cannot resume; the file only gets in the way.
        if installed.contains(&app_name) {
            lgd.forget_resume(&app_name);
            continue;
        }
        let title = lgd
            .cached_title(&app_name)
            .unwrap_or_else(|| app_name.clone());
        restore_job(
            app,
            &app_name,
            &title,
            None,
            None,
            InstallMode::Fresh { with_dlcs: false },
            Vec::new(),
        );
    }

    app.downloads.refresh_indicator();
    remember(app);
}

fn restore_job(
    app: &Rc<App>,
    app_name: &str,
    title: &str,
    base_path: Option<String>,
    folder: Option<String>,
    mode: InstallMode,
    tags: Vec<String>,
) -> Rc<Job> {
    let job = Rc::new(Job::new(
        app_name,
        title,
        base_path,
        Kind::Download(mode),
        tags,
    ));
    *job.folder.borrow_mut() = folder;
    // Whatever was written down about the components, the files already on
    // disk are the better witness, so a restored job checks them before it
    // carries on. Only an install cares: an update or a repair takes the game
    // as it stands and must not narrow it.
    job.unknown_tags.set(matches!(
        mode,
        InstallMode::Fresh { .. } | InstallMode::Components
    ));
    job.paused.set(true);
    job.paused_look();
    attach(app, &job);
    app.busy
        .borrow_mut()
        .insert(job.app_name.clone(), "Paused".to_string());
    job
}

/// How a move ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Moved {
    Done,
    Cancelled,
}

/// Puts a folder move on the same list as the downloads. Carrying a big game
/// to another disk takes long enough that a modal dialog would mean sitting
/// and watching it.
pub fn move_game(app: &Rc<App>, game: &Game, from: PathBuf, new_base: PathBuf) {
    if app.busy.borrow().contains_key(&game.app_name) {
        app.toast("That game is already busy");
        return;
    }
    let Some(folder) = from.file_name().map(std::ffi::OsStr::to_os_string) else {
        return;
    };
    let target = new_base.join(&folder);
    if target.exists() {
        app.toast(&format!(
            "{} already holds a folder called {}",
            new_base.display(),
            folder.display()
        ));
        return;
    }

    let job = Rc::new(Job::new(
        &game.app_name,
        &game.title,
        None,
        Kind::Move,
        Vec::new(),
    ));
    attach(app, &job);

    let control = Arc::new(Control::default());
    *job.control.borrow_mut() = control.clone();
    job.running_look();
    app.busy
        .borrow_mut()
        .insert(game.app_name.clone(), "Moving…".to_string());
    app.downloads.refresh_indicator();

    let (tx, rx) = async_channel::bounded::<(u64, u64)>(1);
    let reporting = job.clone();
    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        while let Ok((done, total)) = rx.recv().await {
            reporting.show_move(done, total);
            if let Some(app) = weak.upgrade() {
                app.downloads.refresh_indicator();
            }
        }
    });

    let lgd = app.lgd.borrow().clone();
    let app_name = game.app_name.clone();
    let finished = job.clone();
    let weak = Rc::downgrade(app);
    spawn_bg(
        move || {
            move_folder(
                &lgd,
                &app_name,
                &from,
                &target,
                &new_base,
                &control,
                &|done, total| {
                    // A full channel only means the bar has not caught up yet.
                    let _ = tx.try_send((done, total));
                },
            )
        },
        move |result| {
            if let Some(app) = weak.upgrade() {
                finish_move(&app, &finished, result);
            }
        },
    );
}

/// The move itself, in the one order that never loses a game.
///
/// The copy lands first, legendary is pointed at it second, and only then does
/// the original go. Stopped anywhere along the way the game is still installed
/// and still runs; the worst left behind is a folder nobody points at.
fn move_folder(
    lgd: &crate::legendary::Legendary,
    app_name: &str,
    from: &Path,
    to: &Path,
    new_base: &Path,
    control: &Control,
    on_progress: &dyn Fn(u64, u64),
) -> anyhow::Result<Moved> {
    use anyhow::Context;

    std::fs::create_dir_all(new_base)
        .with_context(|| format!("creating {}", new_base.display()))?;

    // Inside one filesystem the whole move is a rename: instant, and with
    // nothing copied there is nothing to undo either. Trying it is cheaper
    // than working out in advance whether it can work.
    if std::fs::rename(from, to).is_ok() {
        lgd.relocate(app_name, new_base)?;
        return Ok(Moved::Done);
    }

    let total = crate::disk::size_on_disk(from);
    on_progress(0, total);
    let copied = crate::disk::copy_tree(from, to, &|| control.was_stopped(), &|done| {
        on_progress(done, total)
    })?;
    if copied == crate::disk::Copied::Cancelled {
        // Half a copy is worth nothing, and the original was never touched.
        let _ = crate::disk::remove_tree(to);
        return Ok(Moved::Cancelled);
    }

    lgd.relocate(app_name, new_base)?;
    crate::disk::remove_tree(from).with_context(|| format!("removing {}", from.display()))?;
    Ok(Moved::Done)
}

fn finish_move(app: &Rc<App>, job: &Rc<Job>, result: anyhow::Result<Moved>) {
    remove(app, job);
    match result {
        Ok(Moved::Done) => {
            app.toast(&format!("Moved {}", job.title));
            // The install path changed, so the library and the open page both
            // have to be read again.
            app.refresh();
            return;
        }
        Ok(Moved::Cancelled) => app.toast(&format!("{} stayed where it was", job.title)),
        Err(e) => app.toast_error("Cannot move the game", &e),
    }
    app.rebuild_game_page();
    app.notify_watchers();
}

/// Works out what a job taken over from legendary was downloading before it
/// carries on. Resuming with the wrong components is worse than not resuming:
/// legendary would treat the missing ones as deselected and drop them.
fn resolve_tags(app: &Rc<App>, job: &Rc<Job>) {
    job.pause.set_sensitive(false);
    job.details.set_label("Checking what is left to download…");

    let lgd = app.lgd.borrow().clone();
    let app_name = job.app_name.clone();
    let weak = Rc::downgrade(app);
    let target = job.clone();
    spawn_bg(
        move || lgd.resumed_tags(&app_name),
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            target.pause.set_sensitive(true);
            match result {
                Ok(found) => {
                    // Union, never a narrower set: dropping a component here
                    // would have legendary delete what it covers.
                    let mut tags = target.tags.borrow().clone();
                    tags.extend(found);
                    tags.sort();
                    tags.dedup();
                    *target.tags.borrow_mut() = tags;
                    target.unknown_tags.set(false);
                    remember(&app);
                    run(&app, &target);
                }
                Err(e) => {
                    app.toast_error("Cannot work out what to resume", &e);
                    target.paused_look();
                }
            }
        },
    );
}

/// Starts the legendary process for a job, whether it is new or resumed.
fn run(app: &Rc<App>, job: &Rc<Job>) {
    let control = Arc::new(Control::default());
    *job.control.borrow_mut() = control.clone();
    job.paused.set(false);
    job.running_look();
    app.busy
        .borrow_mut()
        .insert(job.app_name.clone(), format!("{}…", job.what));
    app.downloads.refresh_indicator();

    // Progress arrives on its own channel; the result channel carries the end.
    let (tx, rx) = async_channel::bounded::<Progress>(1);
    let reporting = job.clone();
    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        while let Ok(update) = rx.recv().await {
            reporting.show_progress(&update);
            if let Some(app) = weak.upgrade() {
                app.downloads.refresh_indicator();
            }
        }
    });

    let lgd = app.lgd.borrow().clone();
    let app_name = job.app_name.clone();
    let base_path = job.base_path.clone();
    // run() only ever drives a download; a move has its own worker.
    let Some(mode) = job.kind.mode() else {
        return;
    };
    let tags = job.tags.borrow().clone();

    // Only a first install has a folder to settle, and only on its first run.
    let folder = job.folder.borrow().clone();
    let unsettled = folder.is_none() && matches!(mode, InstallMode::Fresh { .. });
    let epic_folder = if unsettled {
        epic_folder(app, job)
    } else {
        None
    };
    let base = install_base(app, job);
    let discord = app.cfg.borrow().downloads.discord_folders;
    let title = job.title.clone();
    let (settled_tx, settled_rx) = async_channel::bounded::<String>(1);
    let settling = job.clone();
    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        if let Ok(folder) = settled_rx.recv().await {
            *settling.folder.borrow_mut() = Some(folder);
            if let Some(app) = weak.upgrade() {
                remember(&app);
            }
        }
    });

    let finished = job.clone();
    let weak = Rc::downgrade(app);
    spawn_bg(
        move || {
            let folder = folder.or_else(|| {
                let epic = epic_folder?;
                // A download already begun under Epic's name carries on there,
                // and a folder that is already taken is never moved into: it
                // is not Divine's, and cancelling the download would delete it.
                let started = base.join(&epic).is_dir();
                let picked = (discord && !started)
                    .then(|| crate::discord::install_folder(&lgd, &app_name, &title, &epic))
                    .flatten()
                    .filter(|folder| !base.join(folder).exists());
                let folder = picked.unwrap_or(epic);
                let _ = settled_tx.try_send(folder.clone());
                Some(folder)
            });
            // Settling the folder can take a while, and a pause meanwhile
            // should not have to wait for the download to start.
            if control.was_stopped() {
                return Ok(InstallEnd::Interrupted);
            }

            let report = |update: Progress| {
                // A full channel only means the UI has not caught up yet.
                let _ = tx.try_send(update);
            };
            let place = Place {
                base_path: base_path.as_deref(),
                game_folder: folder.as_deref(),
            };
            lgd.install(&app_name, place, mode, &tags, &control, &report)
        },
        move |result| {
            if let Some(app) = weak.upgrade() {
                finish(&app, &finished, result);
            }
        },
    );
}

fn finish(app: &Rc<App>, job: &Rc<Job>, result: anyhow::Result<InstallEnd>) {
    // Paused: the process is gone, but the job stays around to be resumed.
    if job.paused.get() && matches!(result, Ok(InstallEnd::Interrupted)) {
        job.paused_look();
        app.busy
            .borrow_mut()
            .insert(job.app_name.clone(), "Paused".to_string());
        app.downloads.refresh_indicator();
        return;
    }

    remove(app, job);
    if job.cancelled.get() {
        clear_after_cancel(app, job);
    }
    // A stopped component download leaves legendary a resume file that would
    // silently swallow every later attempt, so it goes with the job.
    if !job.kind.resumable() {
        app.lgd.borrow().forget_resume(&job.app_name);
        if job.cancelled.get() || matches!(result, Ok(InstallEnd::Interrupted)) {
            app.toast(&format!(
                "Stopped changing {} — Verify and repair finishes it",
                job.title
            ));
            app.rebuild_game_page();
            app.notify_watchers();
            return;
        }
    }
    if job.cancelled.get() {
        app.toast(&format!("Cancelled {}", job.title));
    } else {
        match result {
            Ok(InstallEnd::Done) => {
                app.toast(&format!("{} is ready", job.title));
                app.notify(
                    &job.app_name,
                    &format!("{} is ready", job.title),
                    &format!("{} finished.", job.what),
                );
                // Reloading the library rebuilds the open game page with it.
                app.refresh();
                return;
            }
            Ok(InstallEnd::Interrupted) => app.toast(&format!("{} stopped", job.title)),
            Err(e) => {
                app.toast_error("Download failed", &e);
                app.notify(
                    &job.app_name,
                    &format!("{} failed", job.title),
                    "Open Divine to see what went wrong.",
                );
            }
        }
    }
    // Nothing was installed, so only the page's own buttons are out of date.
    app.rebuild_game_page();
    app.notify_watchers();
}

fn toggle_pause(app: &Rc<App>, job: &Rc<Job>) {
    if job.paused.get() {
        if lost_destination(app, job) {
            app.toast(&format!(
                "The folder for {} cannot be found. Connect the drive it is on, then resume.",
                job.title
            ));
            return;
        }
        if job.unknown_tags.get() {
            resolve_tags(app, job);
        } else {
            run(app, job);
        }
        return;
    }
    job.paused.set(true);
    job.details.set_label("Pausing…");
    job.pause.set_sensitive(false);
    job.control.borrow().request_stop();
}

fn cancel(app: &Rc<App>, job: &Rc<Job>) {
    if job.kind == Kind::Move {
        // Stopping a move loses nothing: the game has not been touched yet.
        job.cancelled.set(true);
        job.cancel.set_sensitive(false);
        job.status.set_label(&format!("Cancelling {}", job.title));
        job.control.borrow().request_stop();
        return;
    }
    if !job.kind.resumable() {
        confirm_stop(app, job);
        return;
    }
    // Cancelling takes legendary's resume file with it, and without that file
    // legendary starts the download from the beginning rather than picking up
    // the files already on disk. They are worth nothing once the job is gone,
    // so the only fair way to ask is to say what is about to be thrown away.
    match partial_path(app, job) {
        Some(path) => confirm_discard(app, job, path),
        None => stop(app, job),
    }
}

fn confirm_discard(app: &Rc<App>, job: &Rc<Job>, path: PathBuf) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Cancel {}?", job.title)),
        Some(&format!(
            "{} of it is on disk. legendary cannot pick a cancelled download back \
             up, so those files go with it. Pause instead to carry on later.",
            human_size(crate::disk::size_on_disk(&path))
        )),
    );
    dialog.add_response("keep", "Keep Downloading");
    dialog.add_response("cancel", "Cancel Download");
    dialog.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("keep");

    let weak = Rc::downgrade(app);
    let target = job.clone();
    dialog.choose(
        Some(&app.window),
        gtk::gio::Cancellable::NONE,
        move |response| {
            if response != "cancel" {
                return;
            }
            if let Some(app) = weak.upgrade() {
                target.discard.set(true);
                stop(&app, &target);
            }
        },
    );
}

fn confirm_stop(app: &Rc<App>, job: &Rc<Job>) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Stop changing {}?", job.title)),
        Some(
            "legendary cannot carry on with this later: it compares builds, and this \
             one has not changed. What is downloaded stays on disk, and Verify and \
             repair on the game finishes the job from there.",
        ),
    );
    dialog.add_response("keep", "Keep Going");
    dialog.add_response("stop", "Stop");
    dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("keep");

    let weak = Rc::downgrade(app);
    let target = job.clone();
    dialog.choose(
        Some(&app.window),
        gtk::gio::Cancellable::NONE,
        move |response| {
            if response != "stop" {
                return;
            }
            if let Some(app) = weak.upgrade() {
                stop(&app, &target);
            }
        },
    );
}

fn stop(app: &Rc<App>, job: &Rc<Job>) {
    job.cancelled.set(true);
    // A paused job has no process left to wait for, so it goes right away.
    if job.paused.get() {
        remove(app, job);
        clear_after_cancel(app, job);
        app.toast(&format!("Cancelled {}", job.title));
        app.rebuild_game_page();
        app.notify_watchers();
        return;
    }
    job.status.set_label(&format!("Cancelling {}", job.title));
    job.control.borrow().request_stop();
}

/// A cancelled download leaves two things behind. legendary's resume file is
/// the one that made cancelling look undone: the next start finds it and offers
/// the download all over again. The files follow only when that is what was
/// agreed to in the dialog.
fn clear_after_cancel(app: &Rc<App>, job: &Rc<Job>) {
    app.lgd.borrow().forget_resume(&job.app_name);
    if !job.discard.get() {
        return;
    }
    let Some(path) = partial_path(app, job) else {
        return;
    };
    if let Err(e) = crate::disk::remove_tree(&path) {
        let error = anyhow::Error::new(e).context(format!("removing {}", path.display()));
        app.toast_error("Cannot remove the half-finished download", &error);
    }
}

/// The folder legendary would pick for a game left to itself, out of the
/// game's own metadata.
fn epic_folder(app: &Rc<App>, job: &Job) -> Option<String> {
    app.games
        .borrow()
        .iter()
        .find(|game| game.app_name == job.app_name)
        .map(|game| game.folder.clone())
}

fn install_base(app: &Rc<App>, job: &Job) -> PathBuf {
    match &job.base_path {
        Some(path) => PathBuf::from(path),
        None => app.cfg.borrow().general.install_dir.clone(),
    }
}

/// Whether carrying on would write somewhere the job's folder no longer is,
/// as when the drive it was on has been unplugged.
///
/// legendary trusts its resume file over the disk, so an install resumed into
/// an empty mount point fetches only the files still to come and records the
/// game as complete. An update does the same with or without a resume file,
/// since legendary only checks that it may write next to the folder, and a
/// repair writes into the same place. A first install with no resume file yet has written nothing, and creating
/// its folder is legendary's to do.
fn lost_destination(app: &Rc<App>, job: &Job) -> bool {
    let Some(mode) = job.kind.mode() else {
        return false;
    };
    if matches!(mode, InstallMode::Fresh { .. }) {
        let lgd = app.lgd.borrow();
        return lgd.can_resume(&job.app_name) && !install_base(app, job).is_dir();
    }
    app.games
        .borrow()
        .iter()
        .find(|game| game.app_name == job.app_name)
        .is_some_and(Game::folder_missing)
}

/// Where a half-finished download is sitting, if it is there at all.
///
/// legendary joins the base path with the folder it was given, or else the one
/// out of the game's own metadata, so this has to agree with it exactly. Two
/// things are never handed back, because deleting either would cost someone a
/// game: a folder an installed game calls home, and an empty name, which would
/// resolve to the install directory itself.
fn partial_path(app: &Rc<App>, job: &Job) -> Option<PathBuf> {
    let folder = job
        .folder
        .borrow()
        .clone()
        .or_else(|| epic_folder(app, job))?;
    if folder.is_empty() {
        return None;
    }
    let path = install_base(app, job).join(folder);
    if !path.is_dir() {
        return None;
    }
    let games = app.games.borrow();
    let taken = games.iter().any(|game| {
        game.installed
            .as_ref()
            .is_some_and(|install| Path::new(&install.install_path) == path)
    });
    (!taken).then_some(path)
}

fn remove(app: &Rc<App>, job: &Rc<Job>) {
    app.busy.borrow_mut().remove(&job.app_name);
    app.downloads.jobs.borrow_mut().remove(&job.app_name);
    app.downloads.list.remove(&job.row);
    app.downloads.refresh_indicator();
    remember(app);
}

impl Job {
    fn new(
        app_name: &str,
        title: &str,
        base_path: Option<String>,
        kind: Kind,
        tags: Vec<String>,
    ) -> Self {
        let status = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(32)
            .xalign(0.0)
            .hexpand(true)
            .build();
        let bar = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .build();
        // Same reason as the summary above, one level down: the byte counts and
        // the speed change length constantly.
        let details = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(34)
            .max_width_chars(34)
            .xalign(0.0)
            .build();
        details.add_css_class("dim-label");
        details.add_css_class("numeric");

        let pause = gtk::Button::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text("Pause")
            .valign(gtk::Align::Center)
            .build();
        accessible_label(&pause, "Pause");
        pause.add_css_class("circular");
        pause.add_css_class("flat");

        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel")
            .valign(gtk::Align::Center)
            .build();
        accessible_label(&cancel, "Cancel");
        cancel.add_css_class("circular");
        cancel.add_css_class("flat");

        let top = gtk::Box::builder().spacing(6).build();
        top.append(&status);
        top.append(&pause);
        top.append(&cancel);

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(9)
            .margin_bottom(9)
            .margin_start(12)
            .margin_end(9)
            .width_request(300)
            .build();
        body.append(&top);
        body.append(&bar);
        body.append(&details);

        let row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&body)
            .build();

        Self {
            app_name: app_name.to_string(),
            title: title.to_string(),
            base_path,
            folder: RefCell::new(None),
            kind,
            tags: RefCell::new(tags),
            unknown_tags: Cell::new(false),
            what: kind.verb(),
            control: RefCell::new(Arc::new(Control::default())),
            paused: Cell::new(false),
            cancelled: Cell::new(false),
            discard: Cell::new(false),
            fraction: Cell::new(0.0),
            row,
            status,
            bar,
            details,
            pause,
            cancel,
        }
    }

    fn running_look(&self) {
        self.status
            .set_label(&format!("{} {}", self.what, self.title));
        self.details.set_label("Preparing…");
        self.bar.set_fraction(self.fraction.get());
        self.pause.set_icon_name("media-playback-pause-symbolic");
        self.pause.set_tooltip_text(Some("Pause"));
        accessible_label(&self.pause, "Pause");
        self.pause.set_sensitive(true);
    }

    fn paused_look(&self) {
        self.status.set_label(&format!("{} paused", self.title));
        self.details
            .set_label("Resuming continues where it stopped");
        self.pause.set_icon_name("media-playback-start-symbolic");
        self.pause.set_tooltip_text(Some("Resume"));
        accessible_label(&self.pause, "Resume");
        self.pause.set_sensitive(true);
    }

    fn show_move(&self, done: u64, total: u64) {
        if self.cancelled.get() {
            return;
        }
        let fraction = if total > 0 {
            done as f64 / total as f64
        } else {
            0.0
        };
        self.fraction.set(fraction);
        self.bar.set_fraction(fraction);
        self.details
            .set_label(&format!("{} of {}", human_size(done), human_size(total)));
    }

    fn show_progress(&self, update: &Progress) {
        if self.paused.get() || self.cancelled.get() {
            return;
        }
        self.fraction.set(update.fraction);
        self.bar.set_fraction(update.fraction);
        self.details.set_label(&format!(
            "{:.0} / {:.0} MiB · {:.1} MiB/s{}",
            update.done_mib,
            update.total_mib,
            update.speed_mib,
            if update.eta.is_empty() {
                String::new()
            } else {
                format!(" · ETA {}", update.eta)
            }
        ));
    }
}
