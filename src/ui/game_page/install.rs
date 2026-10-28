//! Getting a game onto the disk and choosing what comes with it: the install
//! dialog, optional components, add-ons, and importing an existing folder.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::legendary::ManifestInfo;
use crate::models::{human_size, Game};
use crate::sdl::Selective;
use crate::ui::{accessible_label, downloads, spawn_bg, window::App};

use super::ELBOW_ROOM;

/// Asks where to put the game, then hands the download to the download centre.
pub(super) fn install_dialog(app: &Rc<App>, game: &Game, button: &gtk::Button) {
    let default_path = app.cfg.borrow().general.install_dir.clone();
    let entry = adw::EntryRow::builder()
        .title("Install into")
        .text(default_path.display().to_string())
        .build();
    let browse = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Choose a Folder")
        .valign(gtk::Align::Center)
        .build();
    browse.add_css_class("flat");
    accessible_label(&browse, "Choose a Folder");
    entry.add_suffix(&browse);

    let group = adw::PreferencesGroup::builder()
        .description("A folder named after the game is created inside this one")
        .build();
    group.add(&entry);

    // Add-ons are what most people want along with the game, so the switch is
    // on; it only shows up for a game that has any.
    let with_dlcs = adw::SwitchRow::builder()
        .title("Install Add-ons")
        .subtitle(format!("{} owned for this game", game.dlcs.len()))
        .active(true)
        .build();
    if !game.dlcs.is_empty() {
        group.add(&with_dlcs);
    }

    // Some games are shipped in parts: language packs, high resolution
    // textures. Whether this one is takes only a local lookup, so the row is
    // there from the start; what is in it arrives later.
    let legendary_config = crate::config::legendary_config_dir();
    let has_components = crate::sdl::has_components(&legendary_config, &game.app_name);
    let components = adw::ExpanderRow::builder()
        .title("Components")
        .subtitle("Looking up what this game ships in…")
        .visible(has_components)
        .sensitive(false)
        .build();
    group.add(&components);

    // Its own label rather than part of the body: only a widget can carry the
    // error and warning styles, and hard coded colours would not follow the theme.
    let space = gtk::Label::builder().wrap(true).xalign(0.0).build();
    space.add_css_class("dim-label");
    let extra = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    extra.append(&group);
    extra.append(&space);

    let dialog = adw::AlertDialog::new(Some(&format!("Install {}?", game.title)), None);
    dialog.set_extra_child(Some(&extra));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("install", "Install");
    dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("install"));
    dialog.set_close_response("cancel");

    // Nobody should be able to start an install before seeing that there was
    // something to choose, so the button waits for the component list. The
    // list is small and cached; the manifest behind the sizes is neither, so
    // it is fetched on its own and fills the numbers in afterwards.
    if has_components {
        dialog.set_response_enabled("install", false);
    }

    let picked: Rc<RefCell<Option<Picked>>> = Rc::new(RefCell::new(None));
    let manifest: Rc<RefCell<Option<ManifestInfo>>> = Rc::new(RefCell::new(None));
    // Two things can hold the Install button: components nobody has seen yet,
    // and a disk the game does not fit on. Both are folded in one place so
    // neither can undo the other.
    let listed = Rc::new(Cell::new(!has_components));

    let refresh = {
        let dialog = dialog.clone();
        let picked = picked.clone();
        let manifest = manifest.clone();
        let space = space.clone();
        let entry = entry.clone();
        let listed = listed.clone();
        move || {
            show_size(
                &dialog,
                &picked,
                &manifest,
                &space,
                &PathBuf::from(entry.text().to_string()),
                listed.get(),
            )
        }
    };
    {
        let refresh = refresh.clone();
        entry.connect_changed(move |_| refresh());
    }

    let app_name = game.app_name.clone();
    let state = picked.clone();
    let known = listed.clone();
    let after_toggle = refresh.clone();
    let after_listing = refresh.clone();
    spawn_bg(
        move || crate::sdl::selective(&legendary_config, &app_name).unwrap_or_default(),
        move |selective| {
            known.set(true);
            let Some(selective) = selective else {
                components.set_visible(false);
                after_listing();
                return;
            };
            let rows = component_rows(&selective, &|_| false, after_toggle);
            for row in &rows {
                components.add_row(row);
            }
            components.set_subtitle("Optional parts of the game, left out unless picked");
            components.set_sensitive(true);
            components.set_visible(true);
            state.replace(Some((selective, rows)));
            after_listing();
        },
    );

    let lgd = app.lgd.borrow().clone();
    let app_name = game.app_name.clone();
    let version = game.available_version.clone();
    let state = picked.clone();
    let sizes = manifest.clone();
    let after_manifest = refresh.clone();
    spawn_bg(
        move || lgd.manifest(&app_name, &version).ok(),
        move |found| {
            *sizes.borrow_mut() = found;
            after_manifest();
            update_component_sizes(&state, &sizes);
        },
    );

    let weak = Rc::downgrade(app);
    let field = entry.clone();
    browse.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let chooser = gtk::FileDialog::builder()
            .title("Install into")
            .modal(true)
            .build();
        let current = std::path::PathBuf::from(field.text().to_string());
        if current.is_dir() {
            chooser.set_initial_folder(Some(&gio::File::for_path(&current)));
        }
        let field = field.clone();
        chooser.select_folder(Some(&app.window), gio::Cancellable::NONE, move |result| {
            if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                field.set_text(&path.display().to_string());
            }
        });
    });

    let weak = Rc::downgrade(app);
    let game = game.clone();
    let button = button.clone();
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "install" {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        let base = entry.text().trim().to_string();
        downloads::start(
            &app,
            &game,
            Some(base),
            crate::legendary::InstallMode::Fresh {
                with_dlcs: with_dlcs.is_active(),
            },
            chosen_tags(&picked),
        );
        mark_busy(&app, &game, &button);
    });
}

/// The component groups a game offers, next to the switches showing them.
type Picked = (Selective, Vec<adw::SwitchRow>);

/// One switch per optional component, left for the caller to place. Which of
/// them start on depends on where they are shown: nothing is picked for a
/// fresh install, whereas a game on disk starts from what it has.
fn component_rows(
    selective: &Selective,
    is_on: &dyn Fn(&crate::sdl::Group) -> bool,
    on_toggle: impl Fn() + Clone + 'static,
) -> Vec<adw::SwitchRow> {
    let mut rows = Vec::new();
    for group in &selective.optional {
        let row = adw::SwitchRow::builder()
            .title(&group.name)
            .subtitle(&group.description)
            .use_markup(false)
            .active(is_on(group))
            .build();
        let toggled = on_toggle.clone();
        row.connect_active_notify(move |_| toggled());
        rows.push(row);
    }
    rows
}

/// Adds what each component costs to its row, once the manifest is in.
fn update_component_sizes(
    picked: &Rc<RefCell<Option<Picked>>>,
    manifest: &Rc<RefCell<Option<ManifestInfo>>>,
) {
    let borrowed = picked.borrow();
    let sizes = manifest.borrow();
    let (Some((selective, rows)), Some(manifest)) = (borrowed.as_ref(), sizes.as_ref()) else {
        return;
    };
    for (group, row) in selective.optional.iter().zip(rows) {
        // What the component adds, so tags the game needs anyway do not count.
        let extra: Vec<String> = group
            .tags
            .iter()
            .filter(|tag| !selective.required.contains(tag))
            .cloned()
            .collect();
        let size = human_size(tags_size(&extra, &manifest.tag_download));
        row.set_subtitle(&if group.description.is_empty() {
            format!("About {size}")
        } else {
            format!("{} About {size}.", group.description)
        });
    }
}

/// The manifest tags to install: everything the game needs, plus whatever
/// components are switched on. Empty while the component list is still loading,
/// which leaves the choice to legendary.
fn chosen_tags(picked: &Rc<RefCell<Option<Picked>>>) -> Vec<String> {
    let borrowed = picked.borrow();
    let Some((selective, rows)) = borrowed.as_ref() else {
        return Vec::new();
    };
    // The empty tag is the untagged part of the game, which is always wanted.
    let mut tags = vec![String::new()];
    tags.extend(selective.required.iter().cloned());
    for (group, row) in selective.optional.iter().zip(rows) {
        if row.is_active() {
            tags.extend(group.tags.iter().cloned());
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

/// What the install will cost and whether the disk can take it. Nothing here
/// is guessed: the sizes come from the manifest and the free space from
/// statvfs, and Install stays off while the two disagree.
fn show_size(
    dialog: &adw::AlertDialog,
    picked: &Rc<RefCell<Option<Picked>>>,
    manifest: &Rc<RefCell<Option<ManifestInfo>>>,
    space: &gtk::Label,
    target: &Path,
    listed: bool,
) {
    for style in ["error", "warning", "dim-label"] {
        space.remove_css_class(style);
    }
    let free = crate::disk::free_space(target);

    let borrowed = manifest.borrow();
    let Some(manifest) = borrowed.as_ref() else {
        // The sizes are still on their way; free space is worth saying already.
        space.add_css_class("dim-label");
        space.set_label(
            &free
                .map(|free| format!("{} free", human_size(free)))
                .unwrap_or_default(),
        );
        dialog.set_response_enabled("install", listed);
        return;
    };

    let tags = chosen_tags(picked);
    let (download, disk) = if tags.is_empty() {
        (manifest.download_size, manifest.disk_size)
    } else {
        (
            tags_size(&tags, &manifest.tag_download),
            tags_size(&tags, &manifest.tag_disk),
        )
    };
    dialog.set_body(&format!(
        "About {} to download, {} on disk.",
        human_size(download),
        human_size(disk)
    ));

    let Some(free) = free else {
        dialog.set_response_enabled("install", listed);
        return;
    };
    match free.checked_sub(disk) {
        None => {
            space.add_css_class("error");
            space.set_label(&format!(
                "Not enough room: {} free, {} short",
                human_size(free),
                human_size(disk - free)
            ));
            dialog.set_response_enabled("install", false);
        }
        Some(left) => {
            space.add_css_class(if left < ELBOW_ROOM {
                "warning"
            } else {
                "dim-label"
            });
            space.set_label(&format!(
                "{} free, {} left afterwards",
                human_size(free),
                human_size(left)
            ));
            dialog.set_response_enabled("install", listed);
        }
    }
}

/// Tag sizes overlap where a file carries more than one, so this is close
/// rather than exact — hence every number around it says "about".
pub(super) fn tags_size(tags: &[String], sizes: &HashMap<String, u64>) -> u64 {
    tags.iter().filter_map(|tag| sizes.get(tag)).sum()
}

/// Reflects a job that has just started on the button that started it. The job
/// itself is driven from the download centre from here on.
pub(super) fn mark_busy(app: &Rc<App>, game: &Game, button: &gtk::Button) {
    if let Some(state) = app.busy.borrow().get(&game.app_name) {
        button.set_sensitive(false);
        button.set_label(state);
    }
}

/// Changing what an installed game is made of. legendary fetches whatever was
/// switched on and deletes the files behind whatever was switched off, so the
/// same download job covers both directions.
pub(super) fn components_dialog(app: &Rc<App>, game: &Game) {
    // No tags recorded means the game was installed whole, so everything it
    // has is present.
    let installed: Vec<String> = game
        .installed
        .as_ref()
        .map(|installed| installed.install_tags.clone())
        .unwrap_or_default();

    let group = adw::PreferencesGroup::builder()
        .description(
            "Turning a component off deletes its files. A change here runs to the \
             end or starts over: legendary cannot pick it up halfway.",
        )
        .build();
    let dialog = adw::AlertDialog::new(
        Some("Components"),
        Some("Looking up what this game ships in…"),
    );
    dialog.set_extra_child(Some(&group));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("apply", "Apply");
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("apply"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("apply", false);

    let picked: Rc<RefCell<Option<Picked>>> = Rc::new(RefCell::new(None));
    let manifest: Rc<RefCell<Option<ManifestInfo>>> = Rc::new(RefCell::new(None));

    let legendary_config = crate::config::legendary_config_dir();
    let app_name = game.app_name.clone();
    let state = picked.clone();
    let sizes = manifest.clone();
    let dialog_ref = dialog.clone();
    let present = installed.clone();
    spawn_bg(
        move || crate::sdl::selective(&legendary_config, &app_name).unwrap_or_default(),
        move |selective| {
            let Some(selective) = selective else {
                dialog_ref.set_body("This game is not shipped in parts.");
                return;
            };
            let refresh = {
                let dialog = dialog_ref.clone();
                let state = state.clone();
                let sizes = sizes.clone();
                let present = present.clone();
                move || show_change(&dialog, &state, &sizes, &present)
            };
            let has = present.clone();
            let rows = component_rows(
                &selective,
                &|group| has.is_empty() || group.tags.iter().any(|tag| has.contains(tag)),
                refresh,
            );
            for row in &rows {
                group.add(row);
            }
            state.replace(Some((selective, rows)));
            show_change(&dialog_ref, &state, &sizes, &present);
        },
    );

    let lgd = app.lgd.borrow().clone();
    let app_name = game.app_name.clone();
    let version = game.available_version.clone();
    let state = picked.clone();
    let sizes = manifest.clone();
    let dialog_ref = dialog.clone();
    let present = installed.clone();
    spawn_bg(
        move || lgd.manifest(&app_name, &version).ok(),
        move |found| {
            *sizes.borrow_mut() = found;
            update_component_sizes(&state, &sizes);
            show_change(&dialog_ref, &state, &sizes, &present);
        },
    );

    let weak = Rc::downgrade(app);
    let owned = game.clone();
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "apply" {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        downloads::start(
            &app,
            &owned,
            None,
            crate::legendary::InstallMode::Components,
            chosen_tags(&picked),
        );
    });
}

/// What applying the current choice would cost, and what it would give back.
/// Apply stays out of reach until the choice is worth acting on.
fn show_change(
    dialog: &adw::AlertDialog,
    picked: &Rc<RefCell<Option<Picked>>>,
    manifest: &Rc<RefCell<Option<ManifestInfo>>>,
    installed: &[String],
) {
    let chosen = chosen_tags(picked);
    if chosen.is_empty() {
        return;
    }
    let borrowed = manifest.borrow();
    let Some(manifest) = borrowed.as_ref() else {
        dialog.set_body("Working out what this would cost…");
        return;
    };

    // Against a game installed whole, everything in the manifest is on disk.
    let present: Vec<String> = if installed.is_empty() {
        manifest.tag_disk.keys().cloned().collect()
    } else {
        installed.to_vec()
    };
    let added: Vec<String> = chosen
        .iter()
        .filter(|tag| !present.contains(tag))
        .cloned()
        .collect();
    let removed: Vec<String> = present
        .iter()
        .filter(|tag| !chosen.contains(tag))
        .cloned()
        .collect();

    let download = human_size(tags_size(&added, &manifest.tag_download));
    let freed = human_size(tags_size(&removed, &manifest.tag_disk));
    let body = match (added.is_empty(), removed.is_empty()) {
        (true, true) => "The game already has exactly this.".to_string(),
        (false, true) => format!("Downloads about {download}."),
        (true, false) => format!("Frees about {freed}."),
        (false, false) => format!("Downloads about {download} and frees about {freed}."),
    };
    dialog.set_body(&body);
    dialog.set_response_enabled("apply", !added.is_empty() || !removed.is_empty());
}

/// The add-ons owned for a game, each with a button to add or remove it. Epic
/// ships them as apps of their own, so installing one is an ordinary download
/// next to the base game.
pub(super) fn dlc_dialog(app: &Rc<App>, game: &Game) {
    let group = adw::PreferencesGroup::builder()
        .title("Add-ons")
        .description("Owned add-ons for this game")
        .build();
    let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
    fill_dlcs(app, &group, &rows, game);

    // An ordinary dialog rather than an AdwPreferencesDialog: this is a list of
    // things the account owns, not a page of settings, and the preferences
    // dialog brings a search box and a view switcher that have nothing to find.
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let view = adw::ToolbarView::builder().content(&page).build();
    view.add_top_bar(&adw::HeaderBar::new());

    let dialog = adw::Dialog::builder()
        .title(&game.title)
        .content_width(480)
        .content_height(400)
        .child(&view)
        .build();
    dialog.present(Some(&app.window));

    // Installing or removing an add-on reloads the library; the dialog is not
    // part of the page that gets rebuilt, so it refills itself here.
    let open = Rc::new(Cell::new(true));
    let closed = open.clone();
    dialog.connect_closed(move |_| closed.set(false));
    let app_name = game.app_name.clone();
    app.watch(move |app| {
        if !open.get() {
            return false;
        }
        if let Some(game) = app
            .games
            .borrow()
            .iter()
            .find(|game| game.app_name == app_name)
        {
            fill_dlcs(app, &group, &rows, game);
        }
        true
    });
}

/// Rebuilds the rows from the game as it stands now.
fn fill_dlcs(
    app: &Rc<App>,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
    game: &Game,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }

    for dlc in &game.dlcs {
        let busy = app.busy.borrow().get(&dlc.app_name).cloned();
        let button = gtk::Button::builder()
            .label(match (&busy, dlc.installed) {
                (Some(state), _) => state.as_str(),
                (None, true) => "Remove",
                (None, false) => "Install",
            })
            .sensitive(busy.is_none())
            .valign(gtk::Align::Center)
            .build();
        if dlc.installed {
            button.add_css_class("destructive-action");
        }
        let row = adw::ActionRow::builder()
            .title(&dlc.title)
            .subtitle(if dlc.installed {
                "Installed"
            } else {
                "Not installed"
            })
            .activatable_widget(&button)
            .use_markup(false)
            .build();
        row.add_suffix(&button);
        group.add(&row);
        rows.borrow_mut().push(row);

        let weak = Rc::downgrade(app);
        let owned = dlc.clone();
        button.connect_clicked(move |button| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if owned.installed {
                confirm_remove_dlc(&app, &owned, button);
                return;
            }
            button.set_sensitive(false);
            // The download centre only ever needs a name and a title.
            downloads::start(
                &app,
                &as_game(&owned),
                None,
                crate::legendary::InstallMode::Fresh { with_dlcs: false },
                Vec::new(),
            );
            button.set_label("Installing…");
        });
    }
}

/// A DLC dressed up as a game, which is all the download centre asks for.
pub(super) fn as_game(dlc: &crate::models::Dlc) -> Game {
    Game {
        app_name: dlc.app_name.clone(),
        title: dlc.title.clone(),
        developer: String::new(),
        description: String::new(),
        namespace: String::new(),
        catalog_id: String::new(),
        release_date: String::new(),
        available_version: String::new(),
        cover: None,
        installed: None,
        cloud_saves: false,
        dlcs: Vec::new(),
        partner: String::new(),
        folder: String::new(),
    }
}

/// Taking an add-on off the disk deletes files, so it is asked about the same
/// way uninstalling the game itself is.
fn confirm_remove_dlc(app: &Rc<App>, dlc: &crate::models::Dlc, button: &gtk::Button) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Remove {}?", dlc.title)),
        Some(
            "The add-on's files are deleted. The game itself is left alone, and the \
             add-on can be installed again later.",
        ),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let weak = Rc::downgrade(app);
    let owned = dlc.clone();
    let button = button.clone();
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "remove" {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        button.set_sensitive(false);
        remove_dlc(&app, &owned);
    });
}

fn remove_dlc(app: &Rc<App>, dlc: &crate::models::Dlc) {
    let lgd = app.lgd.borrow().clone();
    let app_name = dlc.app_name.clone();
    let title = dlc.title.clone();
    app.busy
        .borrow_mut()
        .insert(dlc.app_name.clone(), "Removing…".to_string());
    let weak = Rc::downgrade(app);
    let name = dlc.app_name.clone();
    spawn_bg(
        move || lgd.uninstall(&app_name),
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            app.busy.borrow_mut().remove(&name);
            match result {
                Ok(()) => {
                    app.toast(&format!("Removed {title}"));
                    // Reloading the library is what redraws the add-on rows.
                    app.refresh();
                }
                Err(e) => {
                    app.toast_error("Cannot remove the add-on", &e);
                    app.notify_watchers();
                }
            }
        },
    );
}

/// Asks for the folder a game is already installed in and hands it to legendary.
pub(super) fn import_dialog(app: &Rc<App>, game: &Game) {
    let dialog = gtk::FileDialog::builder()
        .title(format!("Select the folder holding {}", game.title))
        .modal(true)
        .build();

    // Start from a matching install found elsewhere, if there is one.
    if let Some(found) = crate::legendary::foreign_installs()
        .into_iter()
        .find(|entry| entry.app_name == game.app_name)
    {
        dialog.set_initial_folder(Some(&gio::File::for_path(&found.install_path)));
    }

    let weak = Rc::downgrade(app);
    let app_name = game.app_name.clone();
    let title = game.title.clone();
    dialog.select_folder(Some(&app.window), gio::Cancellable::NONE, move |result| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let path = match result {
            Ok(folder) => match folder.path() {
                Some(path) => path,
                None => return,
            },
            // Cancelling is not an error worth reporting.
            Err(_) => return,
        };

        let lgd = app.lgd.borrow().clone();
        let weak = Rc::downgrade(&app);
        spawn_bg(
            move || lgd.import(&app_name, &path.to_string_lossy()),
            move |outcome| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                match outcome {
                    Ok(()) => {
                        app.toast(&format!("Imported {title}"));
                        app.nav.pop();
                        app.refresh();
                    }
                    Err(e) => app.toast_error("Import failed", &e),
                }
            },
        );
    });
}
