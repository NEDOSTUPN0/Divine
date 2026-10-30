//! Main window: a sidebar with library filters next to the cover grid.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config::Config;
use crate::epic::collections;
use crate::legendary::Legendary;
use crate::log::{error, warn};
use crate::models::{self, Game};
use crate::runner::{self, Runner};
use crate::ui::downloads::Downloads;
use crate::ui::{accessible_label, game_card, game_page, login, preferences, spawn_bg};

thread_local! {
    /// GTK owns the widgets, but nothing owns `App` itself. Without a strong
    /// reference here it would be dropped the moment `build` returns, leaving
    /// every signal handler holding a dead `Weak` and silently doing nothing.
    static LIVE: RefCell<Vec<Rc<App>>> = const { RefCell::new(Vec::new()) };
}

/// What the sidebar is showing. The whole library comes first and always
/// exists; a collection row exists only while Epic says the collection does.
///
/// There is no row for installed games: the grid already sorts them to the
/// front, so a filter for them would only hide the rest of the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    All,
    /// By collection id rather than by name, because Epic allows two
    /// collections to share a name.
    Collection(String),
}

impl Filter {
    const FIXED: [(Filter, &'static str, &'static str); 1] =
        [(Filter::All, "Library", "view-grid-symbolic")];
}

/// Returns false once it has nothing left to redraw.
type Watcher = Box<dyn Fn(&Rc<App>) -> bool>;

/// Every key Divine answers to, and the one place they are written down: the
/// shortcuts dialog reads the same list back through the actions.
///
/// Ctrl+W and Ctrl+Q both close the window rather than the application, so a
/// download in flight gets its question whichever one is pressed.
const ACCELS: [(&str, &[&str]); 7] = [
    ("win.search", &["<Control>f"]),
    ("win.refresh", &["<Control>r", "F5"]),
    ("win.collection-new", &["<Control>n"]),
    ("win.preferences", &["<Control>comma"]),
    ("win.show-help-overlay", &["<Control>question"]),
    ("win.menu", &["F10"]),
    ("win.close", &["<Control>w", "<Control>q"]),
];

pub struct App {
    pub window: adw::ApplicationWindow,
    pub root: gtk::Stack,
    pub library_page: OnceCell<adw::NavigationPage>,
    /// The primary menu button, kept so F10 can open it the way it does in
    /// every other GNOME application.
    menu_button: OnceCell<gtk::MenuButton>,
    pub nav: adw::NavigationView,
    pub split: adw::OverlaySplitView,
    pub toasts: adw::ToastOverlay,
    pub stack: gtk::Stack,
    pub flow: gtk::FlowBox,
    pub search: gtk::SearchEntry,
    /// Shown in place of the grid when the library has games but the filter
    /// and the search box between them leave none. What it says depends on
    /// which of the two emptied it, so it is kept rather than built in place.
    no_results: adw::StatusPage,
    /// Kept so a search started from somewhere else — the developer on a game
    /// page — can open the bar it is typed into.
    search_bar: gtk::SearchBar,
    pub downloads: Downloads,
    pub filter: RefCell<Filter>,
    /// The sidebar list, kept so the collections in it can be redrawn when
    /// Epic's answer arrives or the user changes one.
    sidebar: gtk::ListBox,
    /// What each sidebar row selects, in the order the rows are in.
    sidebar_rows: RefCell<Vec<Filter>>,
    /// Set while the sidebar is being redrawn. Putting the selection back is
    /// indistinguishable from a click otherwise, and a click navigates.
    refilling: Cell<bool>,
    /// The account's collections, Favorites included. Empty until fetched, and
    /// empty is also what a signed-out or offline session looks like.
    pub collections: RefCell<Vec<crate::epic::collections::Collection>>,
    pub cfg: RefCell<Config>,
    pub lgd: RefCell<Legendary>,
    pub games: RefCell<Vec<Game>>,
    pub runners: RefCell<Vec<Runner>>,
    /// Process group per running game, so a page rebuilt after navigating away
    /// still knows the game is up and can stop it.
    pub running: RefCell<HashMap<String, i32>>,
    /// Games with a download or removal in flight.
    pub busy: RefCell<HashMap<String, String>>,
    /// Installed games whose folder was not there at the last look, so that a
    /// drive coming or going redraws only the tiles it changed.
    missing: RefCell<HashSet<String>>,
    /// Seconds played per game as Epic counts them, fetched once per run.
    pub playtime: RefCell<HashMap<String, u64>>,
    /// Achievements per namespace, kept for the session: the page asks for them
    /// every time it is built, and the answer costs two requests.
    pub achievements: RefCell<HashMap<String, Option<crate::epic::achievements::Progress>>>,
    /// Set once the question about closing mid-download has been answered.
    quitting: Cell<bool>,
    /// Callbacks run whenever the library reloads. Anything floating above the
    /// window (the add-ons dialog) redraws itself from here; returning false
    /// drops the callback, which is how a closed dialog unsubscribes.
    watchers: RefCell<Vec<Watcher>>,
}

impl App {
    pub fn build(application: &adw::Application) -> Rc<Self> {
        let cfg = Config::load();
        let lgd = Legendary::new(&cfg);

        let root = gtk::Stack::new();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&root));
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Divine")
            .default_width(cfg.window.width)
            .default_height(cfg.window.height)
            .maximized(cfg.window.maximized)
            .content(&toasts)
            .build();

        let flow = gtk::FlowBox::builder()
            .valign(gtk::Align::Start)
            .selection_mode(gtk::SelectionMode::None)
            // Equal cells keep the columns evenly spaced. This is only safe
            // because the tiles no longer expand: they sit centred in their cell
            // at a fixed size instead of being stretched to fill it.
            .homogeneous(true)
            // Centred, because a filling flow box hands the leftover width to the
            // cells a pixel at a time (gtkflowbox.c: extra_per_item plus
            // extra_extra), so the gaps come out uneven and shift with every
            // window resize. Centring caps each cell at the tile's own width and
            // leaves the remainder at the edges, where it reads as a margin.
            .halign(gtk::Align::Center)
            // The default cap of 7 per line leaves huge cells on a wide monitor.
            // Columns still follow the window width; only the cell size is fixed.
            .min_children_per_line(2)
            .max_children_per_line(30)
            .column_spacing(12)
            .row_spacing(12)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        flow.add_css_class("library-grid");

        let this = Rc::new(Self {
            stack: gtk::Stack::new(),
            search: gtk::SearchEntry::builder()
                .placeholder_text("Search by title or developer")
                .width_chars(28)
                .build(),
            search_bar: gtk::SearchBar::new(),
            no_results: adw::StatusPage::new(),
            nav: adw::NavigationView::new(),
            split: adw::OverlaySplitView::new(),
            downloads: Downloads::new(),
            filter: RefCell::new(Filter::All),
            sidebar: gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::Single)
                .build(),
            sidebar_rows: RefCell::new(Vec::new()),
            refilling: Cell::new(false),
            collections: RefCell::new(Vec::new()),
            flow,
            root,
            toasts,
            window,
            library_page: OnceCell::new(),
            menu_button: OnceCell::new(),
            cfg: RefCell::new(cfg),
            lgd: RefCell::new(lgd),
            games: RefCell::new(Vec::new()),
            runners: RefCell::new(Vec::new()),
            running: RefCell::new(HashMap::new()),
            busy: RefCell::new(HashMap::new()),
            missing: RefCell::new(HashSet::new()),
            playtime: RefCell::new(HashMap::new()),
            achievements: RefCell::new(HashMap::new()),
            quitting: Cell::new(false),
            watchers: RefCell::new(Vec::new()),
        });

        LIVE.with(|live| live.borrow_mut().push(this.clone()));
        let weak = Rc::downgrade(&this);
        this.window.connect_close_request(move |_| {
            let Some(app) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            // Closing on top of a download is almost always a slip, and for a
            // component change it costs the whole thing, so it is asked about.
            if !app.quitting.get() && confirm_quit(&app) {
                return glib::Propagation::Stop;
            }
            app.remember_size();
            LIVE.with(|live| live.borrow_mut().retain(|a| !Rc::ptr_eq(a, &app)));
            glib::Propagation::Proceed
        });

        this.install_actions();
        this.build_main_view();
        this.window.present();

        // Anything left downloading last time comes back paused, and anything
        // still downloading now is stopped rather than orphaned on the way out.
        crate::ui::downloads::restore(&this);
        // A game started before Divine was last closed may still be up, and the
        // page for it should offer Stop rather than a second Play.
        game_page::restore_running(&this);
        // umu fetches Proton in the middle of a launch; this is what makes that
        // visible instead of the game just taking longer.
        crate::ui::downloads::watch_proton(&this);
        // The known fixes built into Divine work as they are; a newer list in
        // the repository is picked up quietly and shows on the next page drawn.
        let weak = Rc::downgrade(&this);
        spawn_bg(crate::known_fixes::refresh, move |changed| {
            if let (true, Some(app)) = (changed, weak.upgrade()) {
                app.rebuild_game_page();
            }
        });
        // A drive unplugged or plugged back in while Divine is open changes
        // which games can be played, so the library follows it as it happens.
        match crate::disk::mount_changes() {
            Ok(changes) => {
                let weak = Rc::downgrade(&this);
                glib::spawn_future_local(async move {
                    while changes.recv().await.is_ok() {
                        let Some(app) = weak.upgrade() else {
                            break;
                        };
                        app.recheck_folders();
                    }
                });
            }
            Err(e) => warn!("cannot watch drives come and go: {e:#}"),
        }
        let weak = Rc::downgrade(&this);
        application.connect_shutdown(move |_| {
            if let Some(app) = weak.upgrade() {
                app.downloads.stop_all();
            }
        });

        // A session Divine did not live to close is handed over now, marked
        // unfinished, the way the Windows launcher recovers its own journal.
        if this.cfg.borrow().runner.report_playtime {
            let user_file = this.lgd.borrow().user_file();
            spawn_bg(
                move || crate::epic::playtime::flush(&user_file),
                |result| {
                    if let Err(e) = result {
                        warn!("the journalled playtime is not with Epic yet: {e:#}");
                    }
                },
            );
        }

        // Proton scanning touches the disk only, so it is fine to do up front.
        this.rescan_runners();

        if this.lgd.borrow().is_logged_in() {
            this.show_library();
            this.refresh();
        } else {
            this.show_login();
        }
        this
    }

    /// A toast, with the message taken as plain text.
    ///
    /// AdwToast reads its title as Pango markup, and nearly every message here
    /// carries a game's title: "Mount & Blade" alone is enough to lose the rest
    /// of the line. Nothing Divine says in a toast is markup, so it is turned
    /// off here rather than escaped at thirty call sites.
    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(
            adw::Toast::builder()
                .title(message)
                .use_markup(false)
                .build(),
        );
    }

    /// A toast reaches nobody who is looking at their game instead of the
    /// launcher, so anything worth knowing about goes to the desktop as well
    /// while the window is out of focus. The game's name is the notification
    /// id, so a second word about the same game replaces the first.
    pub fn notify(&self, id: &str, title: &str, body: &str) {
        if self.window.is_active() {
            return;
        }
        let Some(application) = self.window.application() else {
            return;
        };
        let notification = gio::Notification::new(title);
        notification.set_body(Some(body));
        notification.set_default_action("app.present");
        application.send_notification(Some(id), &notification);
    }

    /// Shows the first line of an error in a toast and the rest on stderr.
    pub fn toast_error(&self, context: &str, error: &anyhow::Error) {
        error!("{context}: {error:#}");
        let detail = format!("{error:#}");
        let first = detail.lines().next().unwrap_or(&detail);
        self.toast(&format!("{context}: {first}"));
    }

    /// Writes the window's size down for the next run.
    ///
    /// Read while the window is still up: a closing window reports nothing
    /// useful. A maximised one keeps the size it had before, so unmaximising
    /// on the next run lands back where it was rather than at the default.
    fn remember_size(&self) {
        let maximized = self.window.is_maximized();
        let mut cfg = self.cfg.borrow_mut();
        cfg.window.maximized = maximized;
        if !maximized {
            cfg.window.width = self.window.width();
            cfg.window.height = self.window.height();
        }
        if let Err(e) = cfg.save() {
            warn!("cannot write the window size down: {e:#}");
        }
    }

    pub fn rescan_runners(self: &Rc<Self>) {
        let (dir, show_steam) = {
            let cfg = self.cfg.borrow();
            (cfg.general.proton_dir.clone(), cfg.runner.show_steam_proton)
        };
        let mut found = runner::discover(&dir);
        if !show_steam {
            found.retain(|runner| runner.recommended());
        }
        *self.runners.borrow_mut() = found;
    }

    pub fn show_login(self: &Rc<Self>) {
        if self.root.child_by_name("login").is_none() {
            self.root.add_named(&login::view(self), Some("login"));
        }
        self.root.set_visible_child_name("login");
    }

    pub fn show_library(self: &Rc<Self>) {
        if let Some(page) = self.library_page.get() {
            self.nav.replace(std::slice::from_ref(page));
        }
        self.root.set_visible_child_name("main");
    }

    fn install_actions(self: &Rc<Self>) {
        for (name, handler) in [
            ("refresh", Self::refresh as fn(&Rc<Self>)),
            ("import", Self::import_foreign),
            ("preferences", preferences::show),
            ("logout", Self::logout),
            ("about", Self::show_about),
            ("collection-new", Self::new_collection),
            ("search", Self::start_search),
            ("menu", Self::open_menu),
            ("show-help-overlay", Self::show_shortcuts),
            ("close", Self::close),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    handler(&this);
                }
            });
            self.window.add_action(&action);
        }

        // Renaming and deleting need to say which collection, so these carry
        // its id as the action target.
        for (name, handler) in [
            (
                "collection-rename",
                Self::rename_collection as fn(&Rc<Self>, &str),
            ),
            ("collection-delete", Self::delete_collection),
        ] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, id| {
                let (Some(this), Some(id)) = (weak.upgrade(), id.and_then(|id| id.str())) else {
                    return;
                };
                handler(&this, id);
            });
            self.window.add_action(&action);
        }

        // Clicking a notification should bring the launcher forward, which
        // only works through an action the application itself carries.
        if let Some(application) = self.window.application() {
            let present = gio::SimpleAction::new("present", None);
            let window = self.window.clone();
            present.connect_activate(move |_, _| window.present());
            application.add_action(&present);

            for (action, keys) in ACCELS {
                application.set_accels_for_action(action, keys);
            }
        }
    }

    /// Opens the search bar and puts the cursor in it. Typing anywhere already
    /// does this; the key is for the people who reach for it first.
    fn start_search(self: &Rc<Self>) {
        if let Some(library) = self.library_page.get() {
            self.nav.pop_to_page(library);
        }
        self.search_bar.set_search_mode(true);
        self.search.grab_focus();
    }

    /// The menu lives in the sidebar, which may be out of the way, so this
    /// brings the sidebar back before opening it.
    fn open_menu(self: &Rc<Self>) {
        let Some(button) = self.menu_button.get() else {
            return;
        };
        if !self.split.shows_sidebar() {
            self.split.set_show_sidebar(true);
        }
        button.popup();
    }

    /// Closing through the window rather than the application, so that a
    /// download still running gets its question either way.
    fn close(self: &Rc<Self>) {
        self.window.close();
    }

    fn show_shortcuts(self: &Rc<Self>) {
        shortcuts_dialog().present(Some(&self.window));
    }

    fn build_main_view(self: &Rc<Self>) {
        self.split.set_sidebar(Some(&self.build_sidebar()));
        self.split.set_content(Some(&self.nav));
        self.split.set_min_sidebar_width(200.0);
        self.split.set_max_sidebar_width(260.0);

        // Narrow windows get the sidebar out of the way on their own; it comes
        // back as an overlay through the same toggle button.
        if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 700sp") {
            let breakpoint = adw::Breakpoint::new(condition);
            breakpoint.add_setter(&self.split, "collapsed", Some(&true.to_value()));
            breakpoint.add_setter(&self.split, "show-sidebar", Some(&false.to_value()));
            self.window.add_breakpoint(breakpoint);
        }

        self.root.add_named(&self.split, Some("main"));
        self.build_library_page();
    }

    /// Redraws the sidebar: the two fixed rows, then a row per collection.
    ///
    /// Rebuilt whole rather than patched, because a collection can appear,
    /// vanish or be renamed between two fetches and matching that up row by row
    /// buys nothing. The selection is put back on whatever was selected before,
    /// falling back to All Games when that collection is gone.
    fn fill_sidebar(self: &Rc<Self>) {
        self.refilling.set(true);
        let wanted = self.filter.borrow().clone();
        while let Some(row) = self.sidebar.first_child() {
            self.sidebar.remove(&row);
        }

        let mut rows = Vec::new();
        for (filter, label, icon) in Filter::FIXED {
            self.sidebar.append(&sidebar_row(label, icon));
            rows.push(filter);
        }
        for collection in self.collections.borrow().iter() {
            let icon = if collection.is_favorites() {
                "starred-symbolic"
            } else {
                "folder-symbolic"
            };
            let row = sidebar_row(&collection.name, icon);
            // Favorites is Epic's own and has neither to offer.
            if collection.editable() {
                attach_collection_menu(&row, &collection.id);
            }
            self.sidebar.append(&row);
            rows.push(Filter::Collection(collection.id.clone()));
        }

        let chosen = rows.iter().position(|row| *row == wanted).unwrap_or(0);
        *self.sidebar_rows.borrow_mut() = rows;
        self.sidebar
            .select_row(self.sidebar.row_at_index(chosen as i32).as_ref());
        self.refilling.set(false);
    }

    /// Asks for a name and makes an empty collection.
    fn new_collection(self: &Rc<Self>) {
        if self.collections.borrow().len() >= collections::MAX_COLLECTIONS {
            self.toast(&format!(
                "Epic allows {} collections, Favorites included",
                collections::MAX_COLLECTIONS
            ));
            return;
        }
        name_dialog(self, "New Collection", "Create", "", move |app, name| {
            let user_file = app.lgd.borrow().user_file();
            let inner = Rc::downgrade(app);
            spawn_bg(
                // Only Epic knows the new id, so this one waits for it.
                move || collections::create(&user_file, &name).map(|id| (id, name)),
                move |result| {
                    let Some(app) = inner.upgrade() else {
                        return;
                    };
                    match result {
                        Ok((id, name)) => app.show_collections(|all| {
                            all.push(collections::Collection {
                                id,
                                name,
                                members: Vec::new(),
                            })
                        }),
                        Err(e) => app.toast_error("Cannot make the collection", &e),
                    }
                },
            );
        });
    }

    fn rename_collection(self: &Rc<Self>, id: &str) {
        let Some(collection) = self.collection(id) else {
            return;
        };
        let was = collection.name.clone();
        name_dialog(
            self,
            "Rename Collection",
            "Rename",
            &was,
            move |app, name| {
                let user_file = app.lgd.borrow().user_file();
                let collection = collection.clone();
                let id = collection.id.clone();
                let inner = Rc::downgrade(app);
                let undo = collection.name.clone();

                app.show_collections(|all| rename_locally(all, &id, &name));
                spawn_bg(
                    move || collections::rename(&user_file, &collection, &name),
                    move |result| {
                        let Some(app) = inner.upgrade() else {
                            return;
                        };
                        if let Err(e) = result {
                            app.show_collections(|all| rename_locally(all, &id, &undo));
                            app.toast_error("Cannot rename the collection", &e);
                            app.load_collections();
                        }
                    },
                );
            },
        );
    }

    fn delete_collection(self: &Rc<Self>, id: &str) {
        let Some(collection) = self.collection(id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&format!("Delete {}?", collection.name)),
            Some("The games in it are not touched — a collection is a label, not a folder."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |dialog, answer| {
            dialog.close();
            let Some(this) = weak.upgrade() else {
                return;
            };
            if answer != "delete" {
                return;
            }
            let user_file = this.lgd.borrow().user_file();
            let collection = collection.clone();
            let restore = collection.clone();
            let id = collection.id.clone();
            let inner = Rc::downgrade(&this);

            this.show_collections(|all| all.retain(|c| c.id != id));
            spawn_bg(
                move || collections::remove(&user_file, &collection),
                move |result| {
                    let Some(app) = inner.upgrade() else {
                        return;
                    };
                    if let Err(e) = result {
                        app.show_collections(|all| all.push(restore));
                        app.toast_error("Cannot delete the collection", &e);
                        app.load_collections();
                    }
                },
            );
        });
        dialog.present(Some(&self.window));
    }

    fn collection(&self, id: &str) -> Option<collections::Collection> {
        self.collections
            .borrow()
            .iter()
            .find(|c| c.id == id)
            .cloned()
    }

    /// Changes what Divine is showing and redraws everything that shows it.
    ///
    /// Every edit goes through here before it goes to Epic. A star that waits
    /// for three round trips to fill looks broken, and the request is going to
    /// succeed: it is one field on a list the account already owns. When it
    /// does not, the caller puts the change back and re-reads the truth.
    pub fn show_collections(
        self: &Rc<Self>,
        change: impl FnOnce(&mut Vec<collections::Collection>),
    ) {
        change(&mut self.collections.borrow_mut());
        self.fill_sidebar();
        self.refilter();
        self.notify_watchers();
    }

    /// Fetches the account's collections in the background and redraws the
    /// sidebar with them. Quiet on failure: a library without them still works,
    /// and this runs on every refresh.
    pub fn load_collections(self: &Rc<Self>) {
        let user_file = self.lgd.borrow().user_file();
        let weak = Rc::downgrade(self);
        spawn_bg(
            move || crate::epic::collections::all(&user_file),
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                match result {
                    // A membership dialog left open redraws from here too.
                    Ok(collections) => this.show_collections(|held| *held = collections),
                    Err(e) => warn!("cannot read the collections: {e:#}"),
                }
            },
        );
    }

    fn build_sidebar(self: &Rc<Self>) -> adw::NavigationPage {
        let list = self.sidebar.clone();
        list.add_css_class("navigation-sidebar");
        self.fill_sidebar();

        let weak = Rc::downgrade(self);
        list.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            let picked = this
                .sidebar_rows
                .borrow()
                .get(row.index() as usize)
                .cloned();
            let Some(filter) = picked else {
                return;
            };
            *this.filter.borrow_mut() = filter;
            this.refilter();

            // Only a real click navigates. Redrawing the sidebar puts the
            // selection back, and that must not throw away the page the user is
            // on — starring a game from it redraws the sidebar.
            if this.refilling.get() {
                return;
            }

            // The sidebar drives the pane beside it, so picking from it has to
            // come back to the library. Without this the grid quietly refilters
            // behind whichever game page is open and nothing appears to happen.
            if let Some(library) = this.library_page.get() {
                this.nav.pop_to_page(library);
            }
            // On a narrow window the sidebar is an overlay over that pane, and
            // a choice made there is finished with.
            if this.split.is_collapsed() {
                this.split.set_show_sidebar(false);
            }
        });

        let menu = gio::Menu::new();
        menu.append(Some("New Collection…"), Some("win.collection-new"));
        menu.append(Some("Refresh Library"), Some("win.refresh"));
        menu.append(Some("Import Installed Games"), Some("win.import"));
        menu.append(Some("Preferences"), Some("win.preferences"));
        menu.append(Some("Sign Out"), Some("win.logout"));
        menu.append(Some("Keyboard Shortcuts"), Some("win.show-help-overlay"));
        menu.append(Some("About Divine"), Some("win.about"));

        let collapse = gtk::Button::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Hide Sidebar")
            .build();
        accessible_label(&collapse, "Hide Sidebar");
        let split = self.split.clone();
        collapse.connect_clicked(move |_| split.set_show_sidebar(false));

        let primary = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main Menu")
            .build();
        accessible_label(&primary, "Main Menu");
        let _ = self.menu_button.set(primary.clone());

        let header = adw::HeaderBar::new();
        header.pack_start(&collapse);
        header.pack_end(&primary);

        let view = adw::ToolbarView::builder()
            .content(&gtk::ScrolledWindow::builder().child(&list).build())
            .build();
        view.add_top_bar(&header);
        view.add_bottom_bar(&self.downloads.bar);

        adw::NavigationPage::builder()
            .title("Divine")
            .child(&view)
            .build()
    }

    fn build_library_page(self: &Rc<Self>) {
        let header = adw::HeaderBar::new();

        // Only shown while the sidebar is away, since that is the one place left
        // to bring it back from.
        let reveal = gtk::Button::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Show Sidebar")
            .build();
        accessible_label(&reveal, "Show Sidebar");
        let split = self.split.clone();
        reveal.connect_clicked(move |_| split.set_show_sidebar(true));
        self.split
            .bind_property("show-sidebar", &reveal, "visible")
            .invert_boolean()
            .sync_create()
            .build();
        header.pack_start(&reveal);

        let search_bar = self.search_bar.clone();
        search_bar.set_child(Some(&self.search));
        search_bar.connect_entry(&self.search);
        // Typing anywhere in the window opens the bar, as everywhere else in GNOME.
        search_bar.set_key_capture_widget(Some(&self.window));

        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search the Library")
            .build();
        accessible_label(&search_button, "Search the Library");
        search_button
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();
        header.pack_end(&search_button);

        // Closing the bar has to drop the filter with it.
        let entry = self.search.clone();
        search_bar.connect_search_mode_enabled_notify(move |bar| {
            if !bar.is_search_mode() {
                entry.set_text("");
            }
        });

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&self.flow)
            .build();

        self.stack.add_named(&loading_page(), Some("loading"));
        self.stack.add_named(&scroller, Some("library"));
        self.stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("applications-games-symbolic")
                .title("Nothing to Show")
                .description("Refreshing the library may help.")
                .build(),
            Some("empty"),
        );
        self.stack.add_named(&self.no_results, Some("no-results"));
        self.stack.set_visible_child_name("loading");

        let view = adw::ToolbarView::builder().content(&self.stack).build();
        view.add_top_bar(&header);
        view.add_top_bar(&search_bar);

        let page = adw::NavigationPage::builder()
            .title("Library")
            .tag("library")
            .child(&view)
            .build();
        let _ = self.library_page.set(page.clone());
        self.nav.replace(&[page]);

        self.connect_filters();
    }

    /// Whether the sidebar and the search box between them let this game
    /// through. The grid filters with it, and so does the count that decides
    /// whether there is a grid worth showing at all.
    fn shows(&self, game: &Game) -> bool {
        match &*self.filter.borrow() {
            Filter::All => {}
            Filter::Collection(id) => {
                let held = self
                    .collections
                    .borrow()
                    .iter()
                    .any(|c| c.id == *id && c.holds(&game.namespace));
                if !held {
                    return false;
                }
            }
        }
        game.matches(&self.search.text())
    }

    /// Runs the filter over the grid again and puts up the page that fits what
    /// is left. A search or a collection that matches nothing has to say so:
    /// an empty grid on its own reads as a launcher that has stopped working.
    pub fn refilter(self: &Rc<Self>) {
        self.flow.invalidate_filter();
        if self.games.borrow().is_empty() {
            // A library that has not arrived yet is loading, not empty. Only
            // the load itself gets to say which of the two it turned out to be.
            let loading = self
                .stack
                .visible_child_name()
                .is_some_and(|at| at == "loading");
            if !loading {
                self.stack.set_visible_child_name("empty");
            }
            return;
        }
        if self.games.borrow().iter().any(|game| self.shows(game)) {
            self.stack.set_visible_child_name("library");
            return;
        }

        // Two ways to end up here, and they want different advice.
        let searching = !self.search.text().trim().is_empty();
        if searching {
            self.no_results
                .set_icon_name(Some("system-search-symbolic"));
            self.no_results.set_title("No Results Found");
            self.no_results
                .set_description(Some("Try a different search."));
        } else {
            self.no_results.set_icon_name(Some("folder-symbolic"));
            self.no_results.set_title("Collection Is Empty");
            self.no_results
                .set_description(Some("Add games to it from their own pages."));
        }
        self.stack.set_visible_child_name("no-results");
    }

    fn connect_filters(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.flow.set_filter_func(move |child| {
            let Some(this) = weak.upgrade() else {
                return true;
            };
            let games = this.games.borrow();
            let Some(game) = games.get(child.index() as usize) else {
                return true;
            };
            this.shows(game)
        });

        let weak = Rc::downgrade(self);
        self.flow.connect_child_activated(move |_, child| {
            if let Some(this) = weak.upgrade() {
                this.open_game(child.index() as usize);
            }
        });

        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.refilter();
            }
        });
    }

    /// Opens the library filtered by `text`, from wherever the user is.
    ///
    /// A search started elsewhere — the developer under a game's title — means
    /// the whole library, so any collection filter goes with it. Otherwise the
    /// answer would silently be narrowed to the collection the sidebar happened
    /// to be on.
    pub fn search_for(self: &Rc<Self>, text: &str) {
        *self.filter.borrow_mut() = Filter::All;
        self.fill_sidebar();

        if let Some(library) = self.library_page.get() {
            self.nav.pop_to_page(library);
        }
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }

        self.search_bar.set_search_mode(true);
        self.search.set_text(text);
        // The entry only reports a change after a pause, and the grid should
        // already be showing the answer by the time the page settles.
        self.refilter();
        self.search.grab_focus();
    }

    pub fn open_game(self: &Rc<Self>, index: usize) {
        if let Some(page) = game_page::page(self, index) {
            self.nav.push(&page);
        }
    }

    /// Reloads the catalogue and the install list from legendary.
    pub fn refresh(self: &Rc<Self>) {
        self.stack.set_visible_child_name("loading");
        let lgd = self.lgd.borrow().clone();
        let weak = Rc::downgrade(self);
        spawn_bg(
            move || {
                let installed = lgd.list_installed().unwrap_or_default();
                // Add-ons never come back from `list`, only from the cache it
                // fills, so the two sources are stitched together here.
                let dlcs = lgd.cached_dlcs();
                match lgd.list() {
                    Ok(mut list) => {
                        list.extend(dlcs);
                        Ok((models::merge(list, installed), None))
                    }
                    // Falling back to legendary's own metadata cache keeps the
                    // library usable when Epic (or the network) is down.
                    Err(online_err) => match lgd.list_cached() {
                        Ok(mut cached) if !cached.is_empty() => Ok((
                            models::merge(
                                {
                                    cached.extend(lgd.cached_dlcs());
                                    cached
                                },
                                installed,
                            ),
                            Some("Epic is unreachable, showing the local cache".to_string()),
                        )),
                        _ => Err(online_err),
                    },
                }
            },
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok((games, warning)) => {
                        // Filled in the background so that opening a game page
                        // finds the store's answer already on disk: a page that
                        // knows everything about a game before it is drawn does
                        // not rearrange itself a moment later.
                        crate::epic::store::warm(
                            games
                                .iter()
                                .map(|game| (game.namespace.clone(), game.title.clone()))
                                .collect(),
                        );
                        *this.games.borrow_mut() = games;
                        this.populate();
                        this.load_collections();
                        this.rebuild_game_page();
                        this.notify_watchers();
                        if let Some(warning) = warning {
                            this.toast(&warning);
                        }
                    }
                    Err(e) => {
                        this.stack.set_visible_child_name("empty");
                        this.toast_error("Cannot load the library", &e);
                    }
                }
            },
        );
    }

    pub fn watch(self: &Rc<Self>, watcher: impl Fn(&Rc<App>) -> bool + 'static) {
        self.watchers.borrow_mut().push(Box::new(watcher));
    }

    /// Runs the watchers outside the borrow, since one of them may add another.
    pub fn notify_watchers(self: &Rc<Self>) {
        let watchers = std::mem::take(&mut *self.watchers.borrow_mut());
        let alive: Vec<_> = watchers
            .into_iter()
            .filter(|watcher| watcher(self))
            .collect();
        self.watchers.borrow_mut().extend(alive);
    }

    /// Rebuilds the game page that is open, if any, against the current data.
    /// A download that ends while the user is looking at its game should turn
    /// the button into Play, not throw them back to the library.
    pub fn rebuild_game_page(self: &Rc<Self>) {
        let Some(tag) = self.nav.visible_page().and_then(|page| page.tag()) else {
            return;
        };
        let index = self
            .games
            .borrow()
            .iter()
            .position(|game| game.app_name == tag);
        let (Some(index), Some(library)) = (index, self.library_page.get()) else {
            return;
        };
        let Some(page) = game_page::page(self, index) else {
            return;
        };
        self.nav.replace(&[library.clone(), page]);
    }

    fn populate(self: &Rc<Self>) {
        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }
        *self.missing.borrow_mut() = missing_folders(&self.games.borrow());
        for game in self.games.borrow().iter() {
            self.flow.append(&game_card::build(game));
        }
        // The load is over, whatever it turned up, so the spinner comes down
        // here; refilter picks which of the three pages takes its place.
        self.stack.set_visible_child_name("library");
        self.refilter();
    }

    /// Redraws what a drive coming or going changed: the tile of each game whose
    /// folder appeared or vanished, and its page if that is the one showing.
    /// Everything else stays as it is, scroll position included.
    fn recheck_folders(self: &Rc<Self>) {
        let now = missing_folders(&self.games.borrow());
        let before = self.missing.replace(now.clone());
        let flipped: HashSet<&String> = now.symmetric_difference(&before).collect();
        if flipped.is_empty() {
            return;
        }
        for (index, game) in self.games.borrow().iter().enumerate() {
            if !flipped.contains(&game.app_name) {
                continue;
            }
            if let Some(tile) = self.flow.child_at_index(index as i32) {
                tile.set_child(Some(&game_card::build(game)));
            }
        }
        let showing = self.nav.visible_page().and_then(|page| page.tag());
        if showing.is_some_and(|tag| flipped.contains(&tag.to_string())) {
            self.rebuild_game_page();
        }
    }

    /// Imports games installed through a system legendary or through Heroic.
    fn import_foreign(self: &Rc<Self>) {
        let lgd = self.lgd.borrow().clone();
        let weak = Rc::downgrade(self);
        spawn_bg(
            move || {
                let known = lgd.list_installed().unwrap_or_default();
                let mut imported = 0usize;
                let mut errors = Vec::new();
                for entry in crate::legendary::foreign_installs() {
                    if known.iter().any(|k| k.app_name == entry.app_name) {
                        continue;
                    }
                    match lgd.import(&entry.app_name, &entry.install_path) {
                        Ok(()) => imported += 1,
                        Err(e) => errors.push(format!("{}: {e:#}", entry.title)),
                    }
                }
                (imported, errors)
            },
            move |(imported, errors)| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                for error in &errors {
                    error!("import failed: {error:#}");
                }
                match (imported, errors.len()) {
                    (0, 0) => this.toast("Nothing to import"),
                    (n, 0) => {
                        this.toast(&format!("Imported {n} game(s)"));
                        this.refresh();
                    }
                    (n, failed) => {
                        this.toast(&format!("Imported {n} game(s), {failed} failed"));
                        this.refresh();
                    }
                }
            },
        );
    }

    fn logout(self: &Rc<Self>) {
        let lgd = self.lgd.borrow().clone();
        let weak = Rc::downgrade(self);
        spawn_bg(
            move || lgd.logout(),
            move |result| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        this.games.borrow_mut().clear();
                        this.populate();
                        this.show_login();
                    }
                    Err(e) => this.toast_error("Sign-out failed", &e),
                }
            },
        );
    }

    fn show_about(self: &Rc<Self>) {
        let about = adw::AboutDialog::builder()
            .application_name("Divine")
            .application_icon(crate::APP_ID)
            .version(crate::VERSION)
            .developer_name("NEDOSTUPN0")
            .license_type(gtk::License::Gpl30)
            .website("https://github.com/NEDOSTUPN0/Divine")
            .comments(
                "A native GTK4 launcher for Epic Games. The store side runs on Legendary, \
                 games run through umu and Proton.\n\nThe launching logic was cross-checked \
                 against Heroic Games Launcher (GPL-3.0), and the window layout follows \
                 Cartridges (GPL-3.0).",
            )
            .build();
        about.present(Some(&self.window));
    }
}

fn missing_folders(games: &[Game]) -> HashSet<String> {
    games
        .iter()
        .filter(|game| game.folder_missing())
        .map(|game| game.app_name.clone())
        .collect()
}

fn loading_page() -> gtk::Widget {
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    let label = gtk::Label::new(Some("Loading your library…"));
    label.add_css_class("dim-label");

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();
    content.append(&spinner);
    content.append(&label);
    content.upcast()
}

/// Asks before closing while something is downloading. Answers true when the
/// window should stay open: either because the question is now on screen, or
/// because the person said to keep downloading.
fn confirm_quit(app: &Rc<App>) -> bool {
    let (titles, restartable) = app.downloads.in_flight();
    if titles.is_empty() {
        return false;
    }

    let mut body = match titles.len() {
        1 => format!("{} is still downloading.", titles[0]),
        n => format!("{n} downloads are still going."),
    };
    body.push_str(if restartable {
        " Adding components cannot be carried on later — closing now means \
         starting that part over, or finishing it with Verify and repair."
    } else {
        " Closing stops it; it carries on from here next time."
    });

    let dialog = adw::AlertDialog::new(Some("Close while downloading?"), Some(&body));
    dialog.add_response("stay", "Keep downloading");
    dialog.add_response("close", "Close anyway");
    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("stay"));
    dialog.set_close_response("stay");

    let weak = Rc::downgrade(app);
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "close" {
            return;
        }
        if let Some(app) = weak.upgrade() {
            app.quitting.set(true);
            app.window.close();
        }
    });
    true
}

/// The keys, as a page somebody can read.
///
/// Every item but the last section is built from its action, so a shortcut and
/// the thing it does cannot drift apart: the accelerator shown is whatever
/// [`ACCELS`] gave the action. Going back is not an action of ours — the
/// navigation view brings those keys with it — so those are spelled out.
fn shortcuts_dialog() -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();

    let library = adw::ShortcutsSection::new(Some("Library"));
    for (title, action) in [
        ("Search", "win.search"),
        ("Refresh Library", "win.refresh"),
        ("New Collection", "win.collection-new"),
    ] {
        library.add(adw::ShortcutsItem::from_action(title, action));
    }
    dialog.add(library);

    let navigation = adw::ShortcutsSection::new(Some("Navigation"));
    navigation.add(adw::ShortcutsItem::new("Go Back", "Escape <Alt>Left"));
    dialog.add(navigation);

    let general = adw::ShortcutsSection::new(Some("General"));
    for (title, action) in [
        ("Main Menu", "win.menu"),
        ("Preferences", "win.preferences"),
        ("Keyboard Shortcuts", "win.show-help-overlay"),
        ("Close Window", "win.close"),
    ] {
        general.add(adw::ShortcutsItem::from_action(title, action));
    }
    dialog.add(general);

    dialog
}

/// One line in the sidebar: an icon and a label, which is all of them.
fn sidebar_row(label: &str, icon: &str) -> gtk::ListBoxRow {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(
        &gtk::Label::builder()
            .label(label)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .build(),
    );
    gtk::ListBoxRow::builder().child(&row).build()
}

/// The same rename to the copy Divine is showing that Epic is being asked for.
fn rename_locally(all: &mut [collections::Collection], id: &str, name: &str) {
    if let Some(collection) = all.iter_mut().find(|c| c.id == id) {
        collection.name = name.to_string();
    }
}

/// A one-entry dialog for a collection name, shared by making and renaming.
fn name_dialog(
    app: &Rc<App>,
    title: &str,
    confirm: &str,
    text: &str,
    done: impl Fn(&Rc<App>, String) + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(title), None);
    let entry = adw::EntryRow::builder().title("Name").text(text).build();

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    list.append(&entry);
    dialog.set_extra_child(Some(&list));

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", confirm);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("confirm"));
    dialog.set_close_response("cancel");

    // Epic answers a longer name with a 400, so it is cut here rather than
    // typed in full and refused. The launcher stops at the same length.
    let alert = dialog.clone();
    entry.connect_changed(move |entry| {
        let text = entry.text();
        if text.chars().count() > collections::MAX_NAME {
            entry.set_text(&text.chars().take(collections::MAX_NAME).collect::<String>());
            entry.set_position(-1);
            return;
        }
        alert.set_response_enabled("confirm", !text.trim().is_empty());
    });
    dialog.set_response_enabled("confirm", !text.trim().is_empty());

    let weak = Rc::downgrade(app);
    dialog.connect_response(None, move |dialog, answer| {
        let name = entry.text().trim().to_string();
        dialog.close();
        let Some(app) = weak.upgrade() else {
            return;
        };
        if answer == "confirm" && !name.is_empty() {
            done(&app, name);
        }
    });
    dialog.present(Some(&app.window));
}

/// A collection row's own menu: rename or delete. The id travels as the action
/// target, since one action serves every row.
///
/// Three ways in, because a right click is only one of them: a long press for a
/// touch screen, and Menu or Shift+F10 for somebody who never touches a pointer.
/// The menu is put where it was asked for, so a keyboard press does not open it
/// under wherever the mouse last happened to be.
fn attach_collection_menu(row: &gtk::ListBoxRow, id: &str) {
    let menu = gio::Menu::new();
    for (label, action) in [
        ("Rename…", "win.collection-rename"),
        ("Delete…", "win.collection-delete"),
    ] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&id.to_variant()));
        menu.append_item(&item);
    }

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(row);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);

    let at = |popover: &gtk::PopoverMenu, x: f64, y: f64| {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    };

    let gesture = gtk::GestureClick::builder()
        .button(gtk::gdk::BUTTON_SECONDARY)
        .build();
    let clicked = popover.clone();
    gesture.connect_pressed(move |_, _, x, y| at(&clicked, x, y));
    row.add_controller(gesture);

    let press = gtk::GestureLongPress::new();
    let held = popover.clone();
    press.connect_pressed(move |_, x, y| at(&held, x, y));
    row.add_controller(press);

    // No pointer to point at, so the menu hangs off the middle of the row.
    let keyed = popover.clone();
    let action = gtk::CallbackAction::new(move |row, _| {
        let (width, height) = (row.width() as f64, row.height() as f64);
        at(&keyed, width / 2.0, height / 2.0);
        glib::Propagation::Stop
    });
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_scope(gtk::ShortcutScope::Local);
    for keys in ["Menu", "<Shift>F10"] {
        if let Some(trigger) = gtk::ShortcutTrigger::parse_string(keys) {
            shortcuts.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action.clone())));
        }
    }
    row.add_controller(shortcuts);
}
