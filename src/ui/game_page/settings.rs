//! The per-game settings dialog: everything that overrides a global preference
//! for one game, plus the facts about the installation itself.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;

use crate::config::GameSettings;
use crate::log::warn;
use crate::models::{human_size, Game};
use crate::runner::{self, Runner};
use crate::ui::{spawn_bg, window::App, wrapping_list};

use super::launch::show_launch_command;
use super::row;

/// Everything a row needs to write itself down: which game it belongs to, and
/// a way back to the launcher holding the config.
///
/// Rows save as they are changed rather than all together when the dialog is
/// closed. Collecting on close meant every row had to be kept alive in one
/// closure, and a session that ended any other way lost the lot.
#[derive(Clone)]
struct Writer {
    app: std::rc::Weak<App>,
    app_name: String,
}

impl Writer {
    fn set(&self, change: impl FnOnce(&mut GameSettings)) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let mut settings = app.cfg.borrow().game(&self.app_name);
        change(&mut settings);
        app.cfg.borrow_mut().set_game(&self.app_name, settings);

        let saved = app.cfg.borrow().save();
        if let Err(e) = saved {
            app.toast_error("Cannot save game settings", &e);
        }
    }

    fn switch(
        &self,
        title: &str,
        subtitle: &str,
        active: bool,
        set: impl Fn(&mut GameSettings, bool) + 'static,
    ) -> adw::SwitchRow {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(active)
            .build();
        let writer = self.clone();
        row.connect_active_notify(move |row| {
            let on = row.is_active();
            writer.set(|settings| set(settings, on));
        });
        row
    }

    /// A three way switch: follow the global setting, force on, or force off.
    fn override_row(
        &self,
        title: &str,
        subtitle: &str,
        value: Option<bool>,
        set: impl Fn(&mut GameSettings, Option<bool>) + 'static,
    ) -> adw::ComboRow {
        let row = adw::ComboRow::builder()
            .title(title)
            .subtitle(subtitle)
            .model(&gtk::StringList::new(&[
                "Use the global setting",
                "Enabled",
                "Disabled",
            ]))
            .selected(match value {
                None => 0,
                Some(true) => 1,
                Some(false) => 2,
            })
            .build();
        let writer = self.clone();
        row.connect_selected_notify(move |row| {
            let value = override_value(row);
            writer.set(|settings| set(settings, value));
        });
        row
    }

    /// A switch for a setting the known fixes can decide. It shows what the
    /// game will get, and only a position that differs from the fix is written
    /// down, so a game nobody touched keeps following the list as it changes.
    fn fixed_switch(
        &self,
        title: &str,
        subtitle: &str,
        value: Option<bool>,
        fixed: bool,
        set: impl Fn(&mut GameSettings, Option<bool>) + 'static,
    ) -> adw::SwitchRow {
        let subtitle = if fixed {
            format!("{subtitle} Known fixes turn it on for this game.")
        } else {
            subtitle.to_string()
        };
        self.switch(
            title,
            &subtitle,
            value.unwrap_or(fixed),
            move |settings, on| set(settings, crate::known_fixes::departure(on, fixed)),
        )
    }

    /// A text field, written when its apply button is pressed rather than on
    /// every keystroke: half a typed path or a half typed variable is neither.
    fn entry(
        &self,
        title: &str,
        text: &str,
        tooltip: &str,
        set: impl Fn(&mut GameSettings, String) + 'static,
    ) -> adw::EntryRow {
        let row = adw::EntryRow::builder().title(title).text(text).build();
        row.set_tooltip_text(Some(tooltip));
        row.set_show_apply_button(true);

        let writer = self.clone();
        row.connect_apply(move |row| {
            let text = row.text().trim().to_string();
            writer.set(|settings| set(settings, text));
        });
        row
    }
}

/// Overrides for this game alone, applied on top of the global settings.
pub(super) fn settings_dialog(app: &Rc<App>, game: &Game) {
    let saved = app.cfg.borrow().game(&game.app_name);
    let runners = app.runners.borrow().clone();
    let write = Writer {
        app: Rc::downgrade(app),
        app_name: game.app_name.clone(),
    };

    let mut labels = vec!["Use the global setting".to_string()];
    labels.extend(runners.iter().map(|r| r.name.clone()));
    let selected = runners
        .iter()
        .position(|r| r.proton_path() == saved.proton)
        .map_or(0, |index| index as u32 + 1);
    let proton = adw::ComboRow::builder()
        .title("Proton")
        .model(&gtk::StringList::new(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .selected(selected)
        .use_markup(false)
        .list_factory(&wrapping_list())
        .build();
    {
        let writer = write.clone();
        proton.connect_selected_notify(move |row| {
            let picked = match row.selected() {
                0 => String::new(),
                index => runners
                    .get(index as usize - 1)
                    .map(Runner::proton_path)
                    .unwrap_or_default(),
            };
            writer.set(|settings| settings.proton = picked);
        });
    }

    let fix = crate::known_fixes::for_game(&game.app_name);
    let fake_epic = write.fixed_switch(
        "Fake Epic Games Launcher",
        "Starts the game through a wrapper named EpicGamesLauncher.exe. \
         Rockstar titles need this or they offer to sell you the game.",
        saved.fake_epic_launcher,
        fix.as_ref().is_some_and(|fix| fix.fake_epic_launcher),
        |settings, value| settings.fake_epic_launcher = value,
    );

    // A packaged Divine ships the wrapper and this never fires. One whose
    // toolchain could not cross compile it fetches it here instead of at
    // launch, where the failure would look like the game refusing to start.
    let weak = Rc::downgrade(app);
    fake_epic.connect_active_notify(move |row| {
        let (Some(app), true) = (weak.upgrade(), row.is_active()) else {
            return;
        };
        if crate::shim::path().is_some() {
            return;
        }
        app.toast("Downloading the Epic Games Launcher wrapper…");
        row.set_sensitive(false);
        let target = row.clone();
        let weak = Rc::downgrade(&app);
        spawn_bg(crate::shim::ensure, move |result| {
            target.set_sensitive(true);
            let Some(app) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(_) => app.toast("The Epic Games Launcher wrapper is ready"),
                Err(e) => {
                    target.set_active(false);
                    app.toast_error("Wrapper download failed", &e);
                }
            }
        });
    });

    let language = write.entry(
        "Preferred language",
        &saved.language,
        "Two letter code, e.g. ru. Empty uses the system locale.",
        |settings, value| settings.language = value,
    );
    let umu_id = write.entry(
        "umu game id",
        &saved.umu_id,
        "Picks the protonfixes umu applies, e.g. umu-271590. Empty looks it up.",
        |settings, value| settings.umu_id = value,
    );
    let args = write.entry(
        "Launch arguments",
        &saved.args,
        "Passed through to the game",
        |settings, value| settings.args = value,
    );
    let env = write.entry(
        "Environment",
        &render_env(&saved.env),
        "Space separated KEY=VALUE pairs",
        |settings, value| settings.env = parse_env(&value),
    );
    let override_username = write.entry(
        "Name games see",
        &saved.override_username,
        "Empty follows the global setting",
        |settings, value| settings.override_username = value,
    );
    let override_exe = write.entry(
        "Run a different executable",
        &saved.override_exe,
        "Path inside the game folder. What to reach for when a game insists on \
         starting a launcher of its own.",
        |settings, value| settings.override_exe = value,
    );
    let offline = write.switch(
        "Play Offline",
        "Starts without asking Epic anything, and without the update check",
        saved.offline,
        |settings, on| settings.offline = on,
    );
    let skip_update_check = write.switch(
        "Allow an Out of Date Build",
        "legendary refuses to start a game Epic has a newer build of, rather \
         than warning. This lets it through, which is what mods pinned to an \
         older version need.",
        saved.skip_update_check,
        |settings, on| settings.skip_update_check = on,
    );

    let launch_command = adw::ActionRow::builder()
        .title("Launch Command")
        .subtitle("What Divine would run, worked out without running it")
        .sensitive(game.is_installed())
        .build();
    let show_command = gtk::Button::builder()
        .label("Show")
        .valign(gtk::Align::Center)
        .build();
    launch_command.add_suffix(&show_command);
    {
        let weak = Rc::downgrade(app);
        let owned = game.clone();
        show_command.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                show_launch_command(&app, &owned);
            }
        });
    }

    let use_umu = write.override_row(
        "umu",
        "Runs the game through umu-run, which brings protonfixes, the Steam Linux \
         Runtime and its own prefix layout. Off means calling Proton directly.",
        saved.use_umu,
        |settings, value| settings.use_umu = value,
    );
    let wayland = write.override_row(
        "Wine Wayland",
        "Experimental. Needs a Proton build that supports it: GE-Proton and CachyOS \
         do, Valve's does not.",
        saved.wayland,
        |settings, value| settings.wayland = value,
    );
    let hdr = write.override_row(
        "HDR",
        "Only reaches the game with Wine Wayland on, and needs a display and a \
         Proton build that support it.",
        saved.hdr,
        |settings, value| settings.hdr = value,
    );
    let eos_overlay = write.override_row(
        "EOS Overlay",
        "Epic's in-game overlay, for the games that use Epic Online Services. \
         One shared download of about 600 MB, then switched on in this game's \
         prefix. DirectX 12 games usually cannot show it under Proton: it opens \
         and takes the input, but nothing is drawn.",
        saved.eos_overlay,
        |settings, value| settings.eos_overlay = value,
    );
    // Same as the global switch, except the answer can come from either level:
    // the game's own override if it has one, the global setting otherwise.
    let global_wayland = app.cfg.borrow().runner.wayland;
    let follow_wayland = {
        let hdr = hdr.clone();
        move |row: &adw::ComboRow| {
            let enabled = match row.selected() {
                1 => true,
                2 => false,
                _ => global_wayland,
            };
            hdr.set_sensitive(enabled);
        }
    };
    follow_wayland(&wayland);
    wayland.connect_selected_notify(follow_wayland);

    let weak = Rc::downgrade(app);
    let owned = game.clone();
    eos_overlay.connect_selected_notify(move |row| {
        let (Some(app), 1) = (weak.upgrade(), row.selected()) else {
            return;
        };
        crate::ui::ensure_eos_overlay(&app);
        // The overlay draws nothing without them, so switching it on brings the
        // fonts along instead of leaving a working-looking setting that is not.
        let prefix = runner::prefix_path(&owned, &app.cfg.borrow());
        if !runner::has_core_fonts(&prefix) {
            install_core_fonts(&app, &owned, None);
        }
    });
    let cloud_saves = write.override_row(
        "Cloud Saves",
        if game.cloud_saves {
            "Sync this game's saves with Epic before it starts and after it exits"
        } else {
            "Epic keeps no saves for this game"
        },
        saved.cloud_saves,
        |settings, value| settings.cloud_saves = value,
    );
    cloud_saves.set_sensitive(game.cloud_saves);

    let core_fonts = core_fonts_row(app, game);
    let gamemode = write.override_row(
        "GameMode",
        "Launch through gamemoderun",
        saved.gamemode,
        |settings, value| settings.gamemode = value,
    );
    let mangohud = write.override_row(
        "MangoHud",
        "Performance overlay",
        saved.mangohud,
        |settings, value| settings.mangohud = value,
    );
    let ntsync = write.override_row(
        "Ntsync",
        "Kernel side synchronisation",
        saved.ntsync,
        |settings, value| settings.ntsync = value,
    );
    let fsync = write.override_row(
        "Fsync",
        "Futex based synchronisation",
        saved.fsync,
        |settings, value| settings.fsync = value,
    );
    let esync = write.override_row(
        "Esync",
        "Eventfd based synchronisation",
        saved.esync,
        |settings, value| settings.esync = value,
    );

    let launching = adw::PreferencesGroup::builder()
        .title("Launching")
        .description("Every setting here falls back to the global one unless changed")
        .build();
    for row in [
        proton.upcast::<gtk::Widget>(),
        use_umu.upcast(),
        wayland.upcast(),
        hdr.upcast(),
        eos_overlay.upcast(),
        cloud_saves.upcast(),
        gamemode.upcast(),
        mangohud.upcast(),
        fake_epic.upcast(),
    ] {
        launching.add(&row);
    }

    let sync = adw::PreferencesGroup::builder()
        .title("Synchronisation")
        .build();
    for row in [
        ntsync.upcast::<gtk::Widget>(),
        fsync.upcast(),
        esync.upcast(),
    ] {
        sync.add(&row);
    }

    let anticheat = adw::PreferencesGroup::builder()
        .title("Anti-cheat")
        .description(
            "Only for games that ship these anti-cheats. Switching one on downloads \
             the runtime once, and Proton mounts it into the prefix.",
        )
        .build();
    for runtime in crate::runtimes::Runtime::ALL {
        anticheat.add(&runtime_row(
            app,
            &write,
            runtime,
            saved.runtime_override(runtime),
            fix.as_ref().is_some_and(|fix| fix.runtime(runtime)),
        ));
    }

    let advanced = adw::PreferencesGroup::builder().title("Advanced").build();
    for row in [
        core_fonts.upcast::<gtk::Widget>(),
        language.upcast(),
        umu_id.upcast(),
        args.upcast(),
        env.upcast(),
        override_exe.upcast(),
        override_username.upcast(),
        offline.upcast(),
        skip_update_check.upcast(),
        launch_command.upcast(),
    ] {
        advanced.add(&row);
    }

    let page = adw::PreferencesPage::new();
    page.add(&launching);
    page.add(&sync);
    page.add(&anticheat);
    page.add(&advanced);

    let dialog = adw::PreferencesDialog::builder().title(&game.title).build();
    dialog.add(&page);
    dialog.present(Some(&app.window));
}

/// Reference information, kept off the page itself.
pub(super) fn details_group(app: &Rc<App>, game: &Game) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Details").build();
    group.add(&row("App name", &game.app_name));
    if !game.developer.is_empty() {
        group.add(&row("Developer", &game.developer));
    }
    if !game.release_date.is_empty() {
        group.add(&row("Released", &game.release_date));
    }

    match &game.installed {
        Some(installed) => {
            group.add(&row("Version", &installed.version));
            group.add(&row("Size", &human_size(installed.install_size)));
            group.add(&row("Location", &installed.install_path));

            let cfg = app.cfg.borrow();
            let prefix = cfg.general.prefixes_dir.join(runner::prefix_name(game));
            group.add(&row("Prefix", &prefix.display().to_string()));
            group.add(&row("Proton", &cfg.runner.proton));
        }
        None => group.add(&row("State", "Not installed")),
    }
    group
}

/// A switch for one anti-cheat runtime. Turning it on fetches the runtime right
/// away, so a launch does not stall on a download nobody asked for.
fn runtime_row(
    app: &Rc<App>,
    write: &Writer,
    runtime: crate::runtimes::Runtime,
    value: Option<bool>,
    fixed: bool,
) -> adw::SwitchRow {
    let row = write.fixed_switch(
        runtime.title(),
        if runtime == crate::runtimes::Runtime::Eac {
            "Needed by games using EasyAntiCheat"
        } else {
            "Needed by games using BattlEye"
        },
        value,
        fixed,
        move |settings, value| settings.set_runtime_override(runtime, value),
    );

    let weak = Rc::downgrade(app);
    row.connect_active_notify(move |row| {
        let (Some(app), true) = (weak.upgrade(), row.is_active()) else {
            return;
        };
        // Already there: still worth a check for a newer build, quietly.
        let announced = !runtime.is_installed();
        if announced {
            app.toast(&format!("Downloading the {}…", runtime.title()));
        }
        row.set_sensitive(false);
        let target = row.clone();
        let weak = Rc::downgrade(&app);
        spawn_bg(
            move || crate::runtimes::update(runtime, &|_, _| {}),
            move |result| {
                target.set_sensitive(true);
                let Some(app) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        if !announced {
                            return;
                        }
                        app.toast(&format!("{} is ready", runtime.title()));
                    }
                    Err(e) => {
                        // A runtime already on disk stays usable, so only a
                        // failed first download switches itself back off.
                        if runtime.is_installed() {
                            warn!("{} update check failed: {e:#}", runtime.title());
                            return;
                        }
                        target.set_active(false);
                        app.toast_error("Runtime download failed", &e);
                    }
                }
            },
        );
    });
    row
}

/// Epic's overlay hooks the game but draws nothing when the prefix has no
/// Microsoft fonts, which looks exactly like a broken overlay: the game pauses,
/// a cursor appears and the screen stays as it was. Heroic's own EOS guide fixes
/// it with the same winetricks verb.
fn core_fonts_row(app: &Rc<App>, game: &Game) -> adw::ActionRow {
    let install = gtk::Button::builder()
        .label("Install")
        .valign(gtk::Align::Center)
        .build();
    let row = adw::ActionRow::builder()
        .title("Core Fonts")
        .subtitle("Microsoft's fonts in this game's prefix. Epic's overlay needs them to draw.")
        .activatable_widget(&install)
        .build();
    row.add_suffix(&install);

    let weak = Rc::downgrade(app);
    let game = game.clone();
    install.connect_clicked(move |install| {
        if let Some(app) = weak.upgrade() {
            install_core_fonts(&app, &game, Some(install.clone()));
        }
    });
    row
}

/// Runs the winetricks verb in the game's prefix. The button, when there is one,
/// says so while it works: this takes the better part of a minute.
fn install_core_fonts(app: &Rc<App>, game: &Game, button: Option<gtk::Button>) {
    let cfg = app.cfg.borrow().clone();
    let owned = game.clone();
    if let Some(button) = &button {
        button.set_sensitive(false);
        button.set_label("Installing…");
    }
    app.toast(&format!("Installing core fonts into {}", game.title));

    let weak = Rc::downgrade(app);
    spawn_bg(
        move || runner::winetricks(&owned, &cfg, "corefonts"),
        move |result| {
            if let Some(button) = button {
                button.set_sensitive(true);
                button.set_label("Install");
            }
            let Some(app) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(()) => app.toast("Core fonts installed"),
                Err(e) => app.toast_error("Cannot install core fonts", &e),
            }
        },
    );
}

fn override_value(row: &adw::ComboRow) -> Option<bool> {
    match row.selected() {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

fn render_env(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
