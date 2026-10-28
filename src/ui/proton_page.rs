//! Preferences page for managing Proton builds: what is installed, and what can
//! be downloaded from GE-Proton and Proton CachyOS.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::log::error;
use crate::models::human_size;
use crate::proton::{self, Release, Source};
use crate::ui::{accessible_label, spawn_bg, window::App};

pub fn page(app: &Rc<App>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Proton")
        .icon_name("application-x-executable-symbolic")
        .build();

    let location = adw::EntryRow::builder()
        .title("Install builds into")
        .text(app.cfg.borrow().general.proton_dir.display().to_string())
        .build();
    let location_group = adw::PreferencesGroup::builder()
        .title("Location")
        .description(
            "Point this at your Steam compatibilitytools.d if you want Steam to see \
             Divine's builds too. Empty restores the default.",
        )
        .build();
    location_group.add(&location);
    page.add(&location_group);

    let weak = Rc::downgrade(app);
    location.connect_apply(move |row| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let text = row.text();
        let dir = if text.trim().is_empty() {
            crate::config::default_proton_dir()
        } else {
            PathBuf::from(text.trim())
        };
        row.set_text(&dir.display().to_string());
        app.cfg.borrow_mut().general.proton_dir = dir;
        if let Err(e) = app.cfg.borrow().save() {
            app.toast_error("Cannot save preferences", &e);
        }
        app.rescan_runners();
    });
    location.set_show_apply_button(true);

    // A list box of our own, because PreferencesGroup offers no way to clear
    // its rows and this list is rebuilt after every install or removal.
    let installed = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    installed.add_css_class("boxed-list");
    let installed_group = adw::PreferencesGroup::builder().title("Installed").build();
    installed_group.add(&installed);
    page.add(&installed_group);
    fill_installed(app, &installed);

    for source in Source::ALL {
        let group = adw::PreferencesGroup::builder()
            .title(source.label())
            .description(source.description())
            .build();
        page.add(&group);
        load_releases(app, &group, source, &installed);
    }

    page
}

fn fill_installed(app: &Rc<App>, list: &gtk::ListBox) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }

    for runner in app.runners.borrow().iter() {
        let Some(path) = runner.path.clone() else {
            continue;
        };
        let row = adw::ActionRow::builder()
            .title(&runner.name)
            .subtitle(runner.location())
            .subtitle_lines(2)
            .use_markup(false)
            .build();

        // Only builds Divine installed can be removed; the rest belong to Steam
        // or the distribution and are none of our business.
        if runner.managed {
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Remove This Build")
                .valign(gtk::Align::Center)
                .build();
            accessible_label(&remove, "Remove This Build");
            remove.add_css_class("flat");

            let weak = Rc::downgrade(app);
            let owner = list.clone();
            let name = runner.name.clone();
            remove.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    confirm_remove(&app, &owner, &name, &path);
                }
            });
            row.add_suffix(&remove);
        }
        list.append(&row);
    }

    if app.runners.borrow().iter().all(|r| r.path.is_none()) {
        list.append(
            &adw::ActionRow::builder()
                .title("No Proton builds found")
                .subtitle("Install one below, or let umu fetch the latest GE-Proton")
                .build(),
        );
    }
}

/// Always ask first.
fn confirm_remove(app: &Rc<App>, list: &gtk::ListBox, name: &str, path: &Path) {
    let body = format!("{name} will be deleted from disk.");
    let dialog = adw::AlertDialog::new(Some("Remove this Proton build?"), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let weak = Rc::downgrade(app);
    let list = list.clone();
    let name = name.to_string();
    let path = path.to_path_buf();
    dialog.choose(Some(&app.window), gio::Cancellable::NONE, move |response| {
        if response != "remove" {
            return;
        }
        let Some(app) = weak.upgrade() else {
            return;
        };
        let managed_dir = app.cfg.borrow().general.proton_dir.clone();
        let weak = Rc::downgrade(&app);
        let list = list.clone();
        let path = path.clone();
        spawn_bg(
            move || proton::remove(&path, &managed_dir),
            move |result| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        app.toast(&format!("Removed {name}"));
                        app.rescan_runners();
                        fill_installed(&app, &list);
                    }
                    Err(e) => app.toast_error("Cannot remove the build", &e),
                }
            },
        );
    });
}

fn load_releases(
    app: &Rc<App>,
    group: &adw::PreferencesGroup,
    source: Source,
    installed: &gtk::ListBox,
) {
    let loading = adw::ActionRow::builder().title("Loading releases…").build();
    group.add(&loading);

    let weak = Rc::downgrade(app);
    let group = group.clone();
    let installed = installed.clone();
    spawn_bg(
        move || proton::fetch_releases(source),
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            group.remove(&loading);
            match result {
                Ok(releases) => {
                    for release in releases {
                        group.add(&release_row(&app, release, &installed));
                    }
                }
                Err(e) => {
                    error!("{e:#}");
                    group.add(
                        &adw::ActionRow::builder()
                            .title("Could not reach GitHub")
                            .subtitle(format!("{e}"))
                            .use_markup(false)
                            .build(),
                    );
                }
            }
        },
    );
}

fn release_row(app: &Rc<App>, release: Release, installed: &gtk::ListBox) -> adw::ActionRow {
    let already_installed = app
        .runners
        .borrow()
        .iter()
        .any(|r| r.name == release.name || release.name.starts_with(&r.name));

    let row = adw::ActionRow::builder()
        .title(&release.name)
        .subtitle(human_size(release.size))
        .use_markup(false)
        .build();

    let progress = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .width_request(120)
        .visible(false)
        .build();
    row.add_suffix(&progress);

    let button = gtk::Button::builder()
        .label(if already_installed {
            "Installed"
        } else {
            "Install"
        })
        .sensitive(!already_installed)
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&button);

    let weak = Rc::downgrade(app);
    let installed = installed.clone();
    button.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        button.set_sensitive(false);
        button.set_label("Installing…");
        progress.set_visible(true);
        progress.set_fraction(0.0);

        let dir = app.cfg.borrow().general.proton_dir.clone();
        let release = release.clone();
        let name = release.name.clone();

        // The worker reports download progress over its own channel; the result
        // channel only carries the final outcome.
        let (tx, rx) = async_channel::bounded::<f64>(1);
        let bar = progress.clone();
        // Also in the download centre, so closing Preferences does not hide a
        // gigabyte still coming down.
        let task = format!("proton-{}", release.name);
        let title = release.name.clone();
        let weak_bar = Rc::downgrade(&app);
        let shown = task.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(fraction) = rx.recv().await {
                bar.set_fraction(fraction);
                if let Some(app) = weak_bar.upgrade() {
                    app.downloads
                        .show_task(&shown, &title, "Downloading", Some(fraction));
                }
            }
        });

        let button = button.clone();
        let progress = progress.clone();
        let weak = Rc::downgrade(&app);
        let installed = installed.clone();
        let (result_tx, result_rx) = async_channel::bounded(1);
        gio::spawn_blocking(move || {
            let report = |done: u64, total: u64| {
                if total > 0 {
                    // A full channel just means the UI has not caught up yet.
                    let _ = tx.try_send(done as f64 / total as f64);
                }
            };
            let outcome = proton::install(&release, &dir, &report);
            let _ = result_tx.send_blocking(outcome);
        });

        gtk::glib::spawn_future_local(async move {
            let outcome = result_rx.recv().await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            progress.set_visible(false);
            app.downloads.finish_task(&task);
            match outcome {
                Ok(Ok(())) => {
                    button.set_label("Installed");
                    app.toast(&format!("Installed {name}"));
                    app.rescan_runners();
                    fill_installed(&app, &installed);
                }
                Ok(Err(e)) => {
                    button.set_label("Install");
                    button.set_sensitive(true);
                    app.toast_error("Install failed", &e);
                }
                Err(_) => {
                    button.set_label("Install");
                    button.set_sensitive(true);
                }
            }
        });
    });

    row
}
