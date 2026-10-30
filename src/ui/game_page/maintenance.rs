//! Looking after a game that is already installed — moving it to another disk,
//! checking its files, syncing its saves, and removing it.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::legendary::SaveSync;
use crate::log::{error, info};
use crate::models::{human_size, Game};
use crate::runner;
use crate::ui::{accessible_label, downloads, spawn_bg, window::App};

use super::ELBOW_ROOM;

/// Asks where the game should live instead, then moves it there.
pub(super) fn move_dialog(app: &Rc<App>, game: &Game) {
    let Some(installed) = game.installed.clone() else {
        return;
    };
    let current = PathBuf::from(&installed.install_path);
    let Some(old_base) = current.parent().map(Path::to_path_buf) else {
        return;
    };

    let entry = adw::EntryRow::builder()
        .title("Move into")
        .text(old_base.display().to_string())
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
        .description("The game's own folder is carried across, keeping its name")
        .build();
    group.add(&entry);

    let space = gtk::Label::builder().wrap(true).xalign(0.0).build();
    let extra = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    extra.append(&group);
    extra.append(&space);

    let dialog = adw::AlertDialog::new(
        Some(&format!("Move {}?", game.title)),
        Some(&format!("{} to move.", human_size(installed.install_size))),
    );
    dialog.set_extra_child(Some(&extra));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("move", "Move");
    dialog.set_response_appearance("move", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("move"));
    dialog.set_close_response("cancel");

    let refresh = {
        let dialog = dialog.clone();
        let space = space.clone();
        let entry = entry.clone();
        let here = current.clone();
        let size = installed.install_size;
        move || {
            show_move_space(
                &dialog,
                &space,
                &here,
                &PathBuf::from(entry.text().to_string()),
                size,
            )
        }
    };
    refresh();
    {
        let refresh = refresh.clone();
        entry.connect_changed(move |_| refresh());
    }

    let weak = Rc::downgrade(app);
    let field = entry.clone();
    browse.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let chooser = gtk::FileDialog::builder()
            .title("Move into")
            .modal(true)
            .build();
        let start = PathBuf::from(field.text().to_string());
        if start.is_dir() {
            chooser.set_initial_folder(Some(&gio::File::for_path(&start)));
        }
        let field = field.clone();
        chooser.select_folder(Some(&app.window), gio::Cancellable::NONE, move |picked| {
            if let Some(path) = picked.ok().and_then(|folder| folder.path()) {
                field.set_text(&path.display().to_string());
            }
        });
    });

    let weak = Rc::downgrade(app);
    let owned = game.clone();
    let field = entry.clone();
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "move" {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        let new_base = PathBuf::from(field.text().to_string());
        if new_base == old_base {
            return;
        }
        downloads::move_game(&app, &owned, current.clone(), new_base);
    });
}

/// Free space, but only when the move would actually take any.
///
/// A move inside one filesystem is a rename, and one between two subvolumes of
/// a single btrfs is a reflink; both cost nothing whatever the game weighs, so
/// a free space figure there would be answering a question nobody asked.
fn show_move_space(
    dialog: &adw::AlertDialog,
    space: &gtk::Label,
    from: &Path,
    to: &Path,
    size: u64,
) {
    for style in ["error", "warning", "dim-label"] {
        space.remove_css_class(style);
    }
    if crate::disk::same_disk(from, to) {
        space.set_visible(false);
        dialog.set_response_enabled("move", true);
        return;
    }
    space.set_visible(true);
    let Some(free) = crate::disk::free_space(to) else {
        space.set_label("");
        dialog.set_response_enabled("move", true);
        return;
    };
    match free.checked_sub(size) {
        None => {
            space.add_css_class("error");
            space.set_label(&format!(
                "Not enough room: {} free, {} short",
                human_size(free),
                human_size(size - free)
            ));
            dialog.set_response_enabled("move", false);
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
            dialog.set_response_enabled("move", true);
        }
    }
}

/// A sync around a launch is a convenience, not a condition: if Epic is down or
/// the game has no saves yet, the game still starts and still runs.
pub(super) fn sync_saves(
    lgd: &crate::legendary::Legendary,
    app_name: &str,
    prefix: &Path,
    mode: SaveSync,
) {
    match lgd.sync_saves(app_name, prefix, mode) {
        Ok(events) => {
            for event in events {
                info!("saves: {event}");
            }
        }
        Err(e) => error!("cannot sync saves for {app_name}: {e:#}"),
    }
}

/// Checks the installed files against Epic's manifest, and offers to redownload
/// whatever no longer matches.
pub(super) fn verify_files(app: &Rc<App>, game: &Game) {
    let lgd = app.lgd.borrow().clone();
    let app_name = game.app_name.clone();
    app.toast(&format!("Checking {}…", game.title));

    // Hashing a big game runs for minutes, so it goes on the same list the
    // downloads are on rather than leaving the launcher looking idle.
    let task = format!("verify-{}", game.app_name);
    app.downloads
        .show_task(&task, &game.title, CHECKING, Some(0.0));

    let (tx, rx) = async_channel::bounded::<f64>(1);
    let shown = task.clone();
    let title = game.title.clone();
    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        while let Ok(fraction) = rx.recv().await {
            let Some(app) = weak.upgrade() else {
                break;
            };
            app.downloads
                .show_task(&shown, &title, CHECKING, Some(fraction));
        }
    });

    let weak = Rc::downgrade(app);
    let owned = game.clone();
    spawn_bg(
        move || {
            lgd.verify(&app_name, &|fraction| {
                // A full channel only means the bar has not caught up yet.
                let _ = tx.try_send(fraction);
            })
        },
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            app.downloads.finish_task(&task);
            match result {
                Ok(report) if report.is_ok() => app.toast(&format!("{} is intact", owned.title)),
                Ok(report) => offer_repair(&app, &owned, report),
                Err(e) => app.toast_error("Cannot check the files", &e),
            }
        },
    );
}

/// What the verify row says while it works. legendary reports a fraction and
/// nothing else, so there are no stages to name.
const CHECKING: &str = "Checking files";

fn offer_repair(app: &Rc<App>, game: &Game, report: crate::legendary::VerifyReport) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Repair {}?", game.title)),
        Some(&format!(
            "{} file(s) do not match and {} are missing. Repairing downloads just those \
             files again, on the version that is installed.",
            report.corrupted, report.missing
        )),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("repair", "Repair");
    dialog.set_response_appearance("repair", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("repair"));
    dialog.set_close_response("cancel");

    let weak = Rc::downgrade(app);
    let game = game.clone();
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "repair" {
            return;
        }
        if let Some(app) = weak.upgrade() {
            downloads::start(
                &app,
                &game,
                None,
                crate::legendary::InstallMode::Repair,
                Vec::new(),
            );
        }
    });
}

/// Syncs saves on demand. Around a launch this happens on its own, so this is
/// for the times a game was played somewhere else in the meantime.
pub(super) fn sync_saves_now(app: &Rc<App>, game: &Game) {
    let lgd = app.lgd.borrow().clone();
    let prefix = runner::prefix_path(game, &app.cfg.borrow());
    let app_name = game.app_name.clone();
    app.toast(&format!("Syncing saves for {}…", game.title));

    let weak = Rc::downgrade(app);
    spawn_bg(
        move || lgd.sync_saves(&app_name, &prefix, crate::legendary::SaveSync::Both),
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(events) => app.toast(&saves_summary(&events)),
                Err(e) => app.toast_error("Cannot sync saves", &e),
            }
        },
    );
}

/// legendary narrates a sync in its log; the last thing it said is the outcome.
fn saves_summary(events: &[String]) -> String {
    match events.last() {
        Some(last) => last.clone(),
        None => "Nothing to sync".to_string(),
    }
}

pub(super) fn confirm_uninstall(app: &Rc<App>, game: &Game) {
    let missing = game.folder_missing();
    let dialog = adw::AlertDialog::new(
        Some(&format!("Uninstall {}?", game.title)),
        Some(if missing {
            "Its folder cannot be found, so nothing is deleted: the game is only \
             marked as not installed. Files on a drive that is not connected stay \
             there and can be imported again."
        } else {
            "The game files are deleted. Saves kept by Epic in the cloud are not touched."
        }),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("uninstall", "Uninstall");
    dialog.set_response_appearance("uninstall", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let weak = Rc::downgrade(app);
    let app_name = game.app_name.clone();
    let title = game.title.clone();
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "uninstall" {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        let lgd = app.lgd.borrow().clone();
        let weak = Rc::downgrade(&app);
        spawn_bg(
            move || {
                if missing {
                    lgd.forget(&app_name)
                } else {
                    lgd.uninstall(&app_name)
                }
            },
            move |result| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        app.toast(&format!("Uninstalled {title}"));
                        app.nav.pop();
                        app.refresh();
                    }
                    Err(e) => app.toast_error("Uninstall failed", &e),
                }
            },
        );
    });
}
