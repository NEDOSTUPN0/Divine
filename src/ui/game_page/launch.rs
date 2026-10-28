//! The Play button and everything about starting a game: what the button says
//! at any moment, the partner-store activation some titles need first, and the
//! command line Divine would run, shown on request.

use std::rc::Rc;

use adw::prelude::*;

use crate::config::GameSettings;
use crate::legendary::{Activation, SaveSync};
use crate::log::info;
use crate::models::Game;
use crate::runner;
use crate::ui::{downloads, spawn_bg, window::App};

use super::install::{import_dialog, install_dialog, mark_busy};
use super::maintenance::sync_saves;

pub(super) fn action_button(app: &Rc<App>, game: &Game) -> gtk::Box {
    let button = gtk::Button::builder().halign(gtk::Align::Start).build();
    button.add_css_class("pill");

    if !game.is_installed() {
        if game.is_partner_hosted() {
            return partner_actions(app, game);
        }
        button.set_label("Install…");
        button.add_css_class("suggested-action");

        let import = gtk::Button::builder()
            .label("Import…")
            .halign(gtk::Align::Start)
            .tooltip_text("Point Divine at a folder where the game is already installed")
            .build();
        import.add_css_class("pill");

        let weak = Rc::downgrade(app);
        let owned = game.clone();
        button.connect_clicked(move |button| {
            if let Some(app) = weak.upgrade() {
                install_dialog(&app, &owned, button);
            }
        });

        let weak = Rc::downgrade(app);
        let owned = game.clone();
        import.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                import_dialog(&app, &owned);
            }
        });

        // A job started elsewhere still has to show here.
        if let Some(state) = app.busy.borrow().get(&game.app_name) {
            button.set_sensitive(false);
            button.set_label(state);
            import.set_sensitive(false);
        }

        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::Start)
            .build();
        row.append(&button);
        row.append(&import);
        return row;
    }

    // A page rebuilt while the game is up has to come back as a Stop button.
    let running = app.running.borrow().contains_key(&game.app_name);
    set_playing(&button, running);

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Start)
        .build();

    // legendary will not start a build Epic has moved past — it exits rather
    // than warning — so Play beside Update would be offering something that
    // cannot work. Update stands in its place instead. A game told to launch
    // out of date keeps both, and a running one keeps its Stop button.
    let must_update = update_replaces_play(
        game,
        running,
        app.cfg.borrow().game(&game.app_name).skip_update_check,
    );
    if !must_update {
        row.append(&button);
    }

    if game.update_available() {
        let update = gtk::Button::builder()
            .label("Update")
            .tooltip_text(if must_update {
                format!(
                    "Epic offers build {}, and legendary will not start the one on \
                     disk. Allow an out of date build in this game's settings puts \
                     Play back.",
                    game.available_version
                )
            } else {
                format!("Epic offers build {}", game.available_version)
            })
            .build();
        update.add_css_class("pill");
        if must_update {
            // Standing in for Play, so it should look like the thing to press.
            update.add_css_class("suggested-action");
        }
        let weak = Rc::downgrade(app);
        let owned = game.clone();
        update.connect_clicked(move |update| {
            if let Some(app) = weak.upgrade() {
                downloads::start(
                    &app,
                    &owned,
                    None,
                    crate::legendary::InstallMode::Update,
                    Vec::new(),
                );
                mark_busy(&app, &owned, update);
            }
        });
        row.append(&update);
    }

    let weak = Rc::downgrade(app);
    let game = game.clone();
    button.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else {
            return;
        };

        // Already running: this is the Stop button.
        if let Some(group) = app.running.borrow().get(&game.app_name).copied() {
            crate::legendary::stop(group, false);
            app.toast("Asked the game to close. Use Force stop if it does not.");
            return;
        }

        let cfg = app.cfg.borrow().clone();
        set_playing(button, true);
        // umu can spend several silent minutes fetching Proton and building the
        // prefix, but only the first time. A prefix already on disk means that
        // is behind us, so the warning stays out of the way from then on.
        let first_run = !runner::prefix_path(&game, &cfg).join("user.reg").is_file();
        app.toast(&if first_run {
            format!(
                "Launching {}. The first run can take a while while Proton is prepared.",
                game.title
            )
        } else {
            format!("Launching {}", game.title)
        });

        let lgd = app.lgd.borrow().clone();
        let launch_game = game.clone();
        let button = button.clone();
        let weak = Rc::downgrade(&app);

        let finished = game.app_name.clone();

        // The worker hands back the process group as soon as it exists.
        let (pid_tx, pid_rx) = async_channel::bounded(1);
        let app_name = game.app_name.clone();
        let pid_app = Rc::downgrade(&app);
        gtk::glib::spawn_future_local(async move {
            if let Ok(group) = pid_rx.recv().await {
                if let Some(app) = pid_app.upgrade() {
                    // Written down as well as held, so that a Divine restarted
                    // while the game is up knows the game is up.
                    crate::running::remember(&app_name, group);
                    app.running.borrow_mut().insert(app_name, group);
                }
            }
        });

        spawn_bg(
            // Assembled on the worker thread: resolving the umu game id is a
            // network call and must not block the UI.
            move || {
                let (wrapper, env) = runner::build_launch(&launch_game, &cfg)?;
                let vars: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
                info!(
                    "launching {} with {} {}",
                    launch_game.app_name,
                    vars.join(" "),
                    wrapper
                );

                let per_game = cfg.game(&launch_game.app_name);
                let args = launch_args(&per_game, &cfg.runner.override_username);

                // Pull first, push after: a stale cloud save must not land on
                // top of a fresh local one, and the save just made must not be
                // overwritten by the cloud copy either.
                let syncing = launch_game.cloud_saves
                    && per_game.cloud_saves.unwrap_or(cfg.runner.cloud_saves);
                let prefix = runner::prefix_path(&launch_game, &cfg);
                if syncing {
                    sync_saves(&lgd, &launch_game.app_name, &prefix, SaveSync::Download);
                }

                let report = |group: i32| {
                    let _ = pid_tx.try_send(group);
                };
                let watch = crate::epic::playtime::Watch::start(&launch_game.app_name);
                let outcome = lgd.launch(&launch_game.app_name, &wrapper, &env, &args, &report);
                // A launch that fails leaves a session of a few seconds, which is
                // not time played. A game that dies after an hour still counts.
                if outcome.ok || watch.seconds() >= 60 {
                    watch.finish(&lgd.user_file(), cfg.runner.report_playtime);
                } else {
                    watch.discard();
                }

                if syncing {
                    sync_saves(&lgd, &launch_game.app_name, &prefix, SaveSync::Upload);
                }
                anyhow::Ok(outcome)
            },
            move |result| {
                set_playing(&button, false);
                let Some(app) = weak.upgrade() else {
                    return;
                };
                app.running.borrow_mut().remove(&finished);
                crate::running::forget(&finished);
                match result {
                    Ok(outcome) if !outcome.ok => {
                        let reason = outcome
                            .log_tail
                            .last()
                            .cloned()
                            .unwrap_or_else(|| "see the terminal output".into());
                        app.toast(&format!("Launch failed: {reason}"));
                    }
                    Ok(_) => {}
                    Err(e) => app.toast_error("Cannot build the launch command", &e),
                }
            },
        );
    });

    row
}

/// Epic sells these but does not host them, so there is no Install to offer.
/// For a Ubisoft title Divine can at least claim the entitlement, which is the
/// step that has to happen before Ubisoft Connect will hand the files over.
fn partner_actions(app: &Rc<App>, game: &Game) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Start)
        .build();

    let note = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .max_width_chars(44)
        .build();
    note.add_css_class("dim-label");

    if game.is_ubisoft() {
        let button = gtk::Button::builder()
            .label(ACTIVATE)
            .halign(gtk::Align::Start)
            .tooltip_text("Claims every unclaimed Ubisoft title in the library, not only this one")
            .build();
        button.add_css_class("pill");
        button.add_css_class("suggested-action");

        let weak = Rc::downgrade(app);
        button.connect_clicked(move |button| {
            if let Some(app) = weak.upgrade() {
                activate_uplay(&app, button);
            }
        });
        row.append(&button);
        note.set_label(
            "Epic only sells this one. Claim it here, then install it from Ubisoft Connect.",
        );
    } else {
        note.set_label(&format!(
            "Epic only sells this one: {} hosts the files, and Divine cannot install it.",
            game.partner
        ));
    }

    row.append(&note);
    row
}

const ACTIVATE: &str = "Activate on Ubisoft";

fn activate_uplay(app: &Rc<App>, button: &gtk::Button) {
    button.set_sensitive(false);
    button.set_label("Activating…");

    let lgd = app.lgd.borrow().clone();
    let button = button.clone();
    let weak = Rc::downgrade(app);
    spawn_bg(
        move || lgd.activate_uplay(),
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            button.set_sensitive(true);
            button.set_label(ACTIVATE);
            match result {
                Ok(Activation::NotLinked) => app.toast(
                    "No Ubisoft account is linked to Epic yet; the page for linking them \
                     is open in your browser",
                ),
                Ok(Activation::NothingLeft) => {
                    app.toast("Every Ubisoft title in the library is already activated")
                }
                Ok(Activation::Claimed { titles, failed }) if failed.is_empty() => {
                    app.toast(&claimed_message(&titles))
                }
                Ok(Activation::Claimed { failed, .. }) => {
                    app.toast(&format!("Could not activate {}", failed.join(", ")))
                }
                Err(e) => app.toast_error("Activation failed", &e),
            }
        },
    );
}

fn claimed_message(titles: &[String]) -> String {
    match titles.len() {
        0 => "Nothing was left to activate".to_string(),
        1 => format!("Activated {} — Ubisoft Connect has the files", titles[0]),
        count => format!("Activated {count} titles — Ubisoft Connect has the files"),
    }
}

/// Whether Update takes Play's place rather than sitting beside it.
///
/// Three ways out: nothing to update, the game is already up and the button is
/// its Stop, or this game is allowed to launch out of date and so Play still
/// leads somewhere.
fn update_replaces_play(game: &Game, running: bool, allowed_out_of_date: bool) -> bool {
    game.update_available() && !running && !allowed_out_of_date
}

/// legendary's own options first, then whatever is meant for the game.
/// argparse would sort them out either way, but keeping the order is what
/// makes the printed command line readable.
fn launch_args(per_game: &GameSettings, global_username: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if per_game.offline {
        args.push("--offline".into());
    }
    if per_game.skip_update_check {
        args.push("--skip-version-check".into());
    }
    if !per_game.override_exe.is_empty() {
        args.push("--override-exe".into());
        args.push(per_game.override_exe.clone());
    }
    let username = match per_game.override_username.as_str() {
        "" => global_username,
        own => own,
    };
    if !username.is_empty() {
        args.push("--override-username".into());
        args.push(username.to_string());
    }
    if !per_game.language.is_empty() {
        args.push("--language".into());
        args.push(per_game.language.clone());
    }
    args.extend(per_game.args.split_whitespace().map(str::to_string));
    args
}

/// Runs the launch as far as working out the command, then stops and shows it.
pub(super) fn show_launch_command(app: &Rc<App>, game: &Game) {
    let cfg = app.cfg.borrow().clone();
    let lgd = app.lgd.borrow().clone();
    let owned = game.clone();
    let weak = Rc::downgrade(app);
    spawn_bg(
        move || {
            let (wrapper, env) = runner::build_launch(&owned, &cfg)?;
            let args = launch_args(&cfg.game(&owned.app_name), &cfg.runner.override_username);
            let printed = lgd.dry_run(&owned.app_name, &wrapper, &args)?;
            Ok::<String, anyhow::Error>(describe_launch(&env, &printed))
        },
        move |result| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(text) => launch_command_dialog(&app, &text),
                Err(e) => app.toast_error("Cannot work out the launch command", &e),
            }
        },
    );
}

/// Both halves of the story. legendary's dry run says what it would exec, but
/// the environment around it is Divine's own and set on legendary's process,
/// so it never appears in that output.
fn describe_launch(env: &[(String, String)], printed: &str) -> String {
    const WANTED: [&str; 4] = [
        "Pre-Launch Command:",
        "Launch parameters:",
        "Working directory:",
        "Environment overrides:",
    ];

    let mut out = String::new();
    if !env.is_empty() {
        out.push_str("Environment Divine sets:\n");
        for (key, value) in env {
            out.push_str(&format!("  {key}={value}\n"));
        }
        out.push('\n');
    }
    for line in printed.lines() {
        for marker in WANTED {
            if let Some(cut) = line.find(marker) {
                out.push_str(&redact(line[cut..].trim()));
                out.push('\n');
            }
        }
    }
    // Nothing matched means legendary refused rather than reported, and what
    // it said instead is the answer.
    if out
        .trim_end()
        .lines()
        .all(|line| line.starts_with("  ") || line.is_empty())
    {
        out.push_str(printed.trim());
    }
    out
}

/// Takes the exchange code out of a command line.
///
/// legendary mints one per launch and hands it to the game as -AUTH_PASSWORD.
/// It is a live credential for the five minutes it lasts, and this text exists
/// to be read out and pasted into bug reports. Nothing is lost by hiding it:
/// the code is spent on the launch it was made for, so the line was never one
/// to run again anyway.
fn redact(line: &str) -> String {
    const SECRET: &str = "-AUTH_PASSWORD=";
    let Some(start) = line.find(SECRET) else {
        return line.to_string();
    };
    let value = start + SECRET.len();
    let end = line[value..]
        .find(char::is_whitespace)
        .map_or(line.len(), |cut| value + cut);
    format!("{}<hidden>{}", &line[..value], &line[end..])
}

fn launch_command_dialog(app: &Rc<App>, text: &str) {
    let body = gtk::Label::builder()
        .label(text)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .build();
    body.add_css_class("monospace");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(420)
        .propagate_natural_height(true)
        .child(&body)
        .build();

    let dialog = adw::AlertDialog::new(
        Some("Launch command"),
        Some("The single use login code Epic issues per launch is left out."),
    );
    dialog.set_extra_child(Some(&scroller));
    dialog.add_response("close", "Close");
    dialog.add_response("copy", "Copy");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");

    let copied = text.to_string();
    let weak = Rc::downgrade(app);
    dialog.connect_response(None, move |_, response| {
        let (Some(app), "copy") = (weak.upgrade(), response) else {
            return;
        };
        app.window.clipboard().set_text(&copied);
        app.toast("Launch command copied");
    });
    dialog.present(Some(&app.window));
}

/// Swaps the primary button between starting and stopping the game.
/// Picks up games that were still running when Divine last closed.
///
/// The launch is not Divine's child any more — nothing to wait on — so the only
/// way to learn that a game has ended is to keep looking. The look is cheap: a
/// read of one file in `/proc`, once every few seconds.
pub fn restore_running(app: &Rc<App>) {
    let mut watching = crate::running::alive();
    if watching.is_empty() {
        return;
    }
    for game in &watching {
        app.running
            .borrow_mut()
            .insert(game.app_name.clone(), game.group);
    }

    let weak = Rc::downgrade(app);
    let mut since_mark = 0u32;
    gtk::glib::timeout_add_seconds_local(WATCH_EVERY, move || {
        let Some(app) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };

        // The journal is moved along at the pace a game Divine did start moves
        // it, so a machine that loses power costs the same either way.
        since_mark += WATCH_EVERY;
        let mark = since_mark >= crate::epic::playtime::CHECKPOINT.as_secs() as u32;
        if mark {
            since_mark = 0;
        }

        watching.retain(|game| {
            if crate::running::is_alive(game) {
                if mark {
                    crate::epic::playtime::checkpoint(&game.app_name);
                }
                return true;
            }
            ended(&app, &game.app_name);
            false
        });

        match watching.is_empty() {
            true => gtk::glib::ControlFlow::Break,
            false => gtk::glib::ControlFlow::Continue,
        }
    });
}

/// How often a game Divine did not start is checked on.
const WATCH_EVERY: u32 = 5;

/// A game Divine was watching from the outside has closed.
fn ended(app: &Rc<App>, app_name: &str) {
    app.running.borrow_mut().remove(app_name);
    crate::running::forget(app_name);
    crate::epic::playtime::close(
        app_name,
        &app.lgd.borrow().user_file(),
        app.cfg.borrow().runner.report_playtime,
    );
    // The page for it, if it is the one open, is showing Stop.
    app.rebuild_game_page();
}

/// Swaps the button between starting and stopping the game.
///
/// Stop is left plain rather than painted red. The red is for a choice that
/// destroys something, and asking a game to close is what closing a game is;
/// Force Stop, which really does lose the unsaved part, keeps the colour.
fn set_playing(button: &gtk::Button, playing: bool) {
    button.set_label(if playing { "Stop" } else { "Play" });
    if playing {
        button.remove_css_class("suggested-action");
    } else {
        button.add_css_class("suggested-action");
    }
}

/// SIGKILL to the whole group, for when the game stopped responding.
///
/// Asked about first: the game is given no chance to write anything down, and
/// a save lost this way is lost for good. Stop, on the page itself, is the one
/// that asks nicely.
pub(super) fn force_stop(app: &Rc<App>, game: &Game) {
    let Some(group) = app.running.borrow().get(&game.app_name).copied() else {
        app.toast("That game is not running");
        return;
    };

    let dialog = adw::AlertDialog::new(
        Some(&format!("Force Stop {}?", game.title)),
        Some(
            "The game is killed outright rather than asked to close, so anything it \
             has not saved is lost. Stop on the game's page asks it to close instead.",
        ),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("stop", "Force Stop");
    dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let weak = Rc::downgrade(app);
    let title = game.title.clone();
    dialog.choose(
        Some(&app.window),
        gtk::gio::Cancellable::NONE,
        move |response| {
            if response != "stop" {
                return;
            }
            let Some(app) = weak.upgrade() else {
                return;
            };
            crate::legendary::stop(group, true);
            app.toast(&format!("Force stopped {title}"));
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::install::as_game;
    use super::*;

    /// The line as legendary really prints it, code shortened. Anything else
    /// on it has to survive, since the point of showing this is reading it.
    #[test]
    fn the_login_code_never_leaves_the_machine() {
        let line = "Launch parameters: umu-run '/Games/Divine/AmongUs/Among Us.exe' \
                    -AUTH_LOGIN=unused -AUTH_PASSWORD=5c76f71d2b0a4c60 \
                    -AUTH_TYPE=exchangecode -epicapp=963137e4";
        let safe = redact(line);
        assert!(!safe.contains("5c76f71d2b0a4c60"));
        assert!(safe.contains("-AUTH_PASSWORD=<hidden>"));
        assert!(safe.contains("-AUTH_TYPE=exchangecode"));
        assert!(safe.contains("Among Us.exe"));
    }

    #[test]
    fn a_command_without_a_code_is_left_alone() {
        let line = "Working directory: /Games/Divine/AmongUs";
        assert_eq!(redact(line), line);
        // And a code at the very end, with nothing after it to keep.
        assert_eq!(redact("x -AUTH_PASSWORD=abc"), "x -AUTH_PASSWORD=<hidden>");
    }

    fn stale_game() -> Game {
        let mut game = as_game(&crate::models::Dlc {
            app_name: "Fowl".into(),
            title: "The Escapists 2".into(),
            installed: true,
        });
        game.available_version = "7045".into();
        game.installed = Some(crate::models::InstalledEntry {
            app_name: "Fowl".into(),
            title: "The Escapists 2".into(),
            install_path: "/Games/Divine/TheEscapists2".into(),
            version: "7044".into(),
            install_size: 0,
            install_tags: Vec::new(),
        });
        game
    }

    #[test]
    fn update_stands_in_for_play_only_when_play_would_fail() {
        let stale = stale_game();
        assert!(update_replaces_play(&stale, false, false));
        // Already running: that button is Stop and must stay.
        assert!(!update_replaces_play(&stale, true, false));
        // Told to launch out of date, so Play leads somewhere after all.
        assert!(!update_replaces_play(&stale, false, true));

        let current = as_game(&crate::models::Dlc {
            app_name: "Fowl".into(),
            title: "The Escapists 2".into(),
            installed: true,
        });
        assert!(!update_replaces_play(&current, false, false));
    }

    #[test]
    fn legendary_options_come_before_the_game_arguments() {
        let per_game = GameSettings {
            offline: true,
            skip_update_check: true,
            override_exe: "Binaries/Win64/game.exe".into(),
            language: "ru".into(),
            args: "-dx11 -windowed".into(),
            ..GameSettings::default()
        };
        assert_eq!(
            launch_args(&per_game, ""),
            [
                "--offline",
                "--skip-version-check",
                "--override-exe",
                "Binaries/Win64/game.exe",
                "--language",
                "ru",
                "-dx11",
                "-windowed",
            ]
        );
        assert!(launch_args(&GameSettings::default(), "").is_empty());
    }

    #[test]
    fn a_game_name_beats_the_global_one() {
        let global = "Anonymous";
        let none = GameSettings::default();
        assert_eq!(
            launch_args(&none, global),
            ["--override-username", "Anonymous"]
        );

        let own = GameSettings {
            override_username: "Someone Else".into(),
            ..GameSettings::default()
        };
        assert_eq!(
            launch_args(&own, global),
            ["--override-username", "Someone Else"]
        );
        // Neither set leaves the argument off, so legendary uses the real name.
        assert!(launch_args(&none, "").is_empty());
    }
}
