//! What Divine and the programs it runs have said, as a page.
//!
//! Divine runs legendary, legendary runs umu, umu runs Proton and the game.
//! When a launch goes wrong the first question is which of them is complaining,
//! so the page keeps them apart: a source is picked from the list and the rest
//! goes quiet. The lines are shown as they arrive, which is the point — this is
//! read while something is going wrong, not afterwards.
//!
//! It lives behind Preferences rather than in the sidebar. Somebody whose games
//! run has no use for it, and somebody whose games do not will go looking.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::log::{self, Level, Record, Source};
use crate::ui::{accessible_label, window::App};

/// How far back the page can be scrolled, matching what the log itself keeps.
/// A TextView holds this happily; it is a few hundred kilobytes of text.
const SHOWN: usize = 4000;

pub fn page(app: &Rc<App>) -> adw::NavigationPage {
    let text = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(12)
        .right_margin(12)
        .top_margin(9)
        .bottom_margin(9)
        .build();
    let probes = Probes::new();
    tags(&text.buffer());
    probes.follow(&text.buffer());

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&text)
        .build();

    // The probes have to be in the tree to be asked what colour the theme made
    // of them; they draw nothing and ask for no space of their own.
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    content.append(&probes.holder);
    content.append(&scroller);

    let sources = source_filter();
    let levels = level_filter();

    let filters = gtk::Box::builder().spacing(6).build();
    // The class is what makes a bar in an AdwToolbarView look like one; the
    // padding that used to be set by hand comes with it.
    filters.add_css_class("toolbar");
    filters.append(&sources);
    filters.append(&levels);

    let view = adw::ToolbarView::builder().content(&content).build();
    let header = adw::HeaderBar::new();

    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy the Log")
        .build();
    accessible_label(&copy, "Copy the Log");
    header.pack_end(&copy);
    view.add_top_bar(&header);
    view.add_top_bar(&filters);

    let shown = Shown {
        text: text.clone(),
        sources: sources.clone(),
        levels: levels.clone(),
    };

    // Whatever happened before the page was opened, then everything after it.
    shown.refill();
    shown.follow();

    let redraw = shown.clone();
    sources.connect_selected_notify(move |_| redraw.refill());
    let redraw = shown.clone();
    levels.connect_selected_notify(move |_| redraw.refill());

    let weak = Rc::downgrade(app);
    let copied = text.clone();
    copy.connect_clicked(move |_| {
        let buffer = copied.buffer();
        let (start, end) = buffer.bounds();
        let all = buffer.text(&start, &end, false);
        copied.clipboard().set_text(&all);
        if let Some(app) = weak.upgrade() {
            app.toast("Copied to the clipboard");
        }
    });

    adw::NavigationPage::builder()
        .title("Logs")
        .tag("logs")
        .child(&view)
        .build()
}

/// The page as the filters and the incoming records see it.
#[derive(Clone)]
struct Shown {
    text: gtk::TextView,
    sources: gtk::DropDown,
    levels: gtk::DropDown,
}

impl Shown {
    /// Draws the whole history again, which is what a change of filter needs.
    fn refill(&self) {
        let buffer = self.text.buffer();
        buffer.set_text("");
        let kept = log::history();
        for record in kept.iter().skip(kept.len().saturating_sub(SHOWN)) {
            self.append(record);
        }
        self.to_the_end();
    }

    /// Adds every record from now on, for as long as the page is open. The
    /// records arrive on whichever thread wrote them, so they are handed to the
    /// main loop through a channel rather than touching the widget directly.
    fn follow(&self) {
        let (tx, rx) = async_channel::unbounded::<Record>();
        log::listen(move |record| tx.send_blocking(record.clone()).is_ok());

        let weak = self.text.downgrade();
        let shown = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(record) = rx.recv().await {
                // The page is gone: dropping the receiver closes the channel,
                // and the next record unsubscribes the listener with it.
                if weak.upgrade().is_none() {
                    break;
                }
                shown.append(&record);
                shown.to_the_end();
            }
        });
    }

    fn append(&self, record: &Record) {
        if !self.wanted(record) {
            return;
        }
        let buffer = self.text.buffer();
        let mut end = buffer.end_iter();
        buffer.insert(&mut end, &format!("{}  ", stamp(record)));

        // The source and the level carry the marks: a wall of text is only
        // readable if the thing being looked for stands out of it.
        let mut end = buffer.end_iter();
        let at = buffer.create_mark(None, &end, true);
        buffer.insert(
            &mut end,
            &format!(
                "{:<10} {:<6} {}\n",
                record.source.name(),
                tag(record.level),
                record.message
            ),
        );
        let from = buffer.iter_at_mark(&at);
        let to = buffer.end_iter();
        buffer.apply_tag_by_name(name(record.level), &from, &to);
        buffer.delete_mark(&at);
    }

    /// Scrolls to the newest line, but only when the newest line was already
    /// what was being read: yanking the view away from somebody scrolled back
    /// through a launch is worse than not following at all.
    fn to_the_end(&self) {
        let Some(scroller) = self
            .text
            .parent()
            .and_downcast::<gtk::ScrolledWindow>()
            .map(|scroller| scroller.vadjustment())
        else {
            return;
        };
        let at_the_end = scroller.value() + scroller.page_size()
            >= scroller.upper() - scroller.page_size() / 4.0;
        if !at_the_end {
            return;
        }
        let text = self.text.clone();
        // After the layout has caught up with the line just added.
        glib::idle_add_local_once(move || {
            let buffer = text.buffer();
            let mut end = buffer.end_iter();
            text.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
        });
    }

    fn wanted(&self, record: &Record) -> bool {
        let source = match self.sources.selected() {
            0 => true,
            picked => match self.sources.model().and_downcast::<gtk::StringList>() {
                Some(list) => list
                    .string(picked)
                    .is_some_and(|name| name == record.source.name()),
                None => true,
            },
        };
        source && record.level >= LEVELS[self.levels.selected() as usize].0
    }
}

/// The lowest level each choice lets through.
const LEVELS: [(Level, &str); 4] = [
    (Level::Debug, "Everything"),
    (Level::Info, "Info and above"),
    (Level::Warn, "Warnings and errors"),
    (Level::Error, "Errors"),
];

fn level_filter() -> gtk::DropDown {
    let names: Vec<&str> = LEVELS.iter().map(|(_, name)| *name).collect();
    let filter = gtk::DropDown::from_strings(&names);
    filter.set_tooltip_text(Some("How much to show"));
    // The list stands on its own with no label beside it, so it says here what
    // it picks. The tooltip is the explanation, not the name.
    accessible_label(&filter, "Detail");
    // Info: the debug lines are every request Divine makes, which drowns out
    // what a launch had to say.
    filter.set_selected(1);
    filter
}

/// Everything that has said anything this session, plus whatever turns up
/// later. A source that has never spoken is not worth a row in the list.
fn source_filter() -> gtk::DropDown {
    let list = gtk::StringList::new(&["All sources"]);
    let filter = gtk::DropDown::builder().model(&list).build();
    filter.set_tooltip_text(Some("Which program to show"));
    accessible_label(&filter, "Program");

    let known: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let add = {
        let list = list.clone();
        let known = known.clone();
        move |source: Source| {
            let name = source.name().to_string();
            let mut known = known.borrow_mut();
            if known.contains(&name) {
                return;
            }
            known.push(name.clone());
            list.append(&name);
        }
    };
    for record in log::history() {
        add(record.source);
    }

    let (tx, rx) = async_channel::unbounded::<Source>();
    log::listen(move |record| tx.send_blocking(record.source).is_ok());
    let weak = filter.downgrade();
    glib::spawn_future_local(async move {
        while let Ok(source) = rx.recv().await {
            if weak.upgrade().is_none() {
                break;
            }
            add(source);
        }
    });

    filter
}

/// The time of day the line was written, which is all a reader needs: the day
/// is today, since the log only holds this run.
fn stamp(record: &Record) -> String {
    let since = record
        .at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default();
    glib::DateTime::from_unix_local(since)
        .and_then(|at| at.format("%H:%M:%S"))
        .map(|at| at.to_string())
        .unwrap_or_default()
}

fn tag(level: Level) -> &'static str {
    match level {
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
    }
}

fn name(level: Level) -> &'static str {
    match level {
        Level::Debug => "level-debug",
        Level::Info => "level-info",
        Level::Warn => "level-warn",
        Level::Error => "level-error",
    }
}

/// One tag per level, with no colour of its own yet: [`Probes`] fills those in
/// from the theme, and refills them whenever it changes.
fn tags(buffer: &gtk::TextBuffer) {
    for level in [Level::Debug, Level::Info, Level::Warn, Level::Error] {
        let tag = gtk::TextTag::builder().name(name(level)).build();
        // Errors are what somebody scrolling this page is looking for.
        if level == Level::Error {
            tag.set_weight(700);
        }
        buffer.tag_table().add(&tag);
    }
}

/// Where the level colours come from.
///
/// A text tag takes a colour and not a style class, so the theme's own has to
/// be read off something first. These boxes carry nothing but the classes in
/// `style.css`: they draw nothing, ask for no space, and exist to be asked what
/// colour they came out. Info is the ordinary case and keeps the plain one.
///
/// Boxes rather than labels for the reason the ProtonDB dot is a dot: a user
/// gtk.css can set `label { color: ... }` at a priority above both the theme
/// and us, and every question would come back the same colour.
#[derive(Clone)]
struct Probes {
    holder: gtk::Box,
    debug: gtk::Box,
    warn: gtk::Box,
    error: gtk::Box,
}

impl Probes {
    fn new() -> Self {
        let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let probe = |class| {
            let probe = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            probe.add_css_class(class);
            holder.append(&probe);
            probe
        };
        Self {
            debug: probe("log-debug"),
            warn: probe("log-warning"),
            error: probe("log-error"),
            holder,
        }
    }

    /// Colours the tags now and again on every change of theme. Light, dark and
    /// high contrast each have their own answer, and the page is often left
    /// open across a switch between them.
    fn follow(&self, buffer: &gtk::TextBuffer) {
        self.paint(buffer);

        // Before the widgets are realised there is no style to read, so the
        // first real answer arrives with the realise rather than from here.
        let probes = self.clone();
        let onto = buffer.clone();
        self.holder.connect_realize(move |_| probes.paint(&onto));

        let manager = adw::StyleManager::default();
        let repaint = {
            let probes = self.clone();
            let onto = buffer.clone();
            let holder = self.holder.downgrade();
            move || {
                // A page that has gone away has nothing left to colour.
                if holder.upgrade().is_some() {
                    probes.paint(&onto);
                }
            }
        };
        let on_dark = repaint.clone();
        manager.connect_dark_notify(move |_| on_dark());
        manager.connect_high_contrast_notify(move |_| repaint());
    }

    fn paint(&self, buffer: &gtk::TextBuffer) {
        let table = buffer.tag_table();
        for (level, probe) in [
            (Level::Debug, &self.debug),
            (Level::Warn, &self.warn),
            (Level::Error, &self.error),
        ] {
            if let Some(tag) = table.lookup(name(level)) {
                tag.set_foreground_rgba(Some(&probe.color()));
            }
        }
    }
}
