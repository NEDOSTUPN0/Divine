//! Playing a game from start to finish: saves down, the session journal,
//! `legendary launch`, saves back up. The Play button and `divine --launch`
//! both come through here, so a game started from a shortcut is counted,
//! synced and remembered exactly like one started from the window.
//!
//! `divine --launch` is the command every shortcut runs. It never opens a
//! window and never registers as the application: it runs the game as its own
//! child and waits on it, so whatever started it — the desktop, or Steam with
//! its overlay and game mode — sees the game for as long as it runs. Anything
//! that needs a person to decide is handed to the window instead.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use gtk::{gio, glib};

use crate::config::{Config, GameSettings};
use crate::legendary::{LaunchArgs, LaunchOutcome, Legendary, SaveSync};
use crate::log::{error, info, warn};
use crate::models::{self, Game};
use crate::runner;
use crate::tray::{Entry, Tray};

/// The flag shortcuts start Divine with.
pub const LAUNCH: &str = "--launch";

/// Runs the game and waits for it. Blocking: call it from a worker thread.
///
/// `started` hears the game's process group as soon as there is one.
/// `skip_update` starts an out of date build this once, as a game's own
/// setting would every time.
pub fn run(
    lgd: &Legendary,
    cfg: &Config,
    game: &Game,
    skip_update: bool,
    started: &dyn Fn(i32),
) -> Result<LaunchOutcome> {
    let (wrapper, env) = runner::build_launch(game, cfg)?;
    let vars: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    info!(
        "launching {} with {} {}",
        game.app_name,
        vars.join(" "),
        wrapper
    );

    let per_game = cfg.game(&game.app_name);
    let mut args = launch_args(&per_game, &cfg.runner.override_username)?;
    if skip_update && !per_game.skip_update_check {
        args.options.push("--skip-version-check".into());
    }

    // Pull first, push after: a stale cloud save must not land on top of a
    // fresh local one, and the save just made must not be overwritten by the
    // cloud copy either.
    let syncing = game.cloud_saves && per_game.cloud_saves.unwrap_or(cfg.runner.cloud_saves);
    let prefix = runner::prefix_path(game, cfg);
    if syncing {
        sync_saves(lgd, &game.app_name, &prefix, SaveSync::Download);
    }

    if let Err(e) = crate::recent::played(&game.app_name) {
        warn!("cannot note {} as played: {e:#}", game.title);
    }
    let account = lgd.account().map(|a| a.id).unwrap_or_default();
    let watch = crate::epic::playtime::Watch::start(&game.app_name, &account);
    let _programs = game
        .installed
        .as_ref()
        .map(|installed| watch_programs(lgd, cfg, &prefix, installed));
    let outcome = lgd.launch(&game.app_name, &wrapper, &env, &args, &|group| {
        // Written down as well as held, so that a window opened, or opened
        // again, while the game is up knows the game is up.
        crate::running::remember(&game.app_name, group);
        started(group);
    });
    crate::running::forget(&game.app_name);
    // A launch that fails leaves a session of a few seconds, which is not time
    // played. A game that dies after an hour still counts.
    if outcome.ok || watch.seconds() >= 60 {
        watch.finish(&lgd.user_file(), cfg.runner.report_playtime);
    } else {
        watch.discard();
    }

    if syncing && !outcome.out_of_date() {
        sync_saves(lgd, &game.app_name, &prefix, SaveSync::Upload);
    }
    Ok(outcome)
}

/// Notes the programs the game runs, and gives each a menu entry of its own
/// the moment it shows up, so that its window has its icon straight away.
fn watch_programs(
    lgd: &Legendary,
    cfg: &Config,
    prefix: &Path,
    installed: &crate::models::InstalledEntry,
) -> crate::programs::Watch {
    let lgd = lgd.clone();
    let cfg = cfg.clone();
    let entry = installed.clone();
    crate::programs::Watch::start(
        &installed.app_name,
        prefix,
        Path::new(&installed.install_path),
        move || {
            if !cfg.general.shortcuts {
                return;
            }
            let game = crate::shortcuts::Shortcut::new(&lgd, &cfg, &entry);
            if let Err(e) = crate::shortcuts::sync_game(&game) {
                warn!(
                    "cannot give {}'s programs entries of their own: {e:#}",
                    game.title
                );
            }
        },
    )
}

/// legendary's own options, kept apart from what is meant for the game: in one
/// list legendary would read some of the game's as its own.
pub fn launch_args(per_game: &GameSettings, global_username: &str) -> Result<LaunchArgs> {
    let mut options: Vec<String> = Vec::new();
    if per_game.offline {
        options.push("--offline".into());
    }
    if per_game.skip_update_check {
        options.push("--skip-version-check".into());
    }
    if !per_game.override_exe.is_empty() {
        options.push("--override-exe".into());
        options.push(per_game.override_exe.clone());
    }
    let username = match per_game.override_username.as_str() {
        "" => global_username,
        own => own,
    };
    if !username.is_empty() {
        options.push("--override-username".into());
        options.push(username.to_string());
    }
    if !per_game.language.is_empty() {
        options.push("--language".into());
        options.push(per_game.language.clone());
    }
    let line = crate::launch_options::parse(&per_game.launch_options)
        .context("reading the launch options")?;
    Ok(LaunchArgs {
        options,
        game: line.args,
    })
}

/// A sync around a launch is a convenience, not a condition: if Epic is down or
/// the game has no saves yet, the game still starts and still runs.
pub fn sync_saves(lgd: &Legendary, app_name: &str, prefix: &Path, mode: SaveSync) {
    match lgd.sync_saves(app_name, prefix, mode) {
        Ok(events) => {
            for event in events {
                info!("saves: {event}");
            }
        }
        Err(e) => error!("cannot sync saves for {app_name}: {e:#}"),
    }
}

/// An installed game, put together from what legendary keeps on disk alone.
///
/// A shortcut cannot wait for the library to load from Epic, and does not need
/// it: whether the build is current is legendary's to say at launch. The game
/// may belong to an account other than the one signed in, so the cached entry
/// is read directly rather than through the active account's library.
pub fn installed_game(lgd: &Legendary, app_name: &str) -> Option<Game> {
    let installed = lgd
        .list_installed()
        .ok()?
        .into_iter()
        .filter(|entry| entry.app_name == app_name)
        .collect();
    let entry = lgd.cached_entry(app_name)?;
    models::merge(vec![entry], installed)
        .into_iter()
        .find(Game::is_installed)
}

/// What the window is asked to do for a game when a shortcut cannot go on.
pub enum Handover {
    /// Offer the update legendary insisted on, then play.
    Update,
    /// Say why the game did not start.
    Problem(String),
}

impl Handover {
    /// The window's command line for it, after `--game <app_name>`.
    fn args(&self) -> Vec<String> {
        match self {
            Handover::Update => vec!["--update".into()],
            Handover::Problem(text) => vec!["--problem".into(), text.clone()],
        }
    }
}

/// `divine --launch <app_name>`.
pub fn from_shortcut(app_name: &str) -> i32 {
    let cfg = Config::load();
    crate::proxy::apply(&cfg.proxy);
    let lgd = Legendary::new(&cfg);

    if !lgd.is_logged_in() {
        return hand_over(app_name, Handover::Problem("Sign in to Epic first.".into()));
    }
    let Some(game) = installed_game(&lgd, app_name) else {
        return hand_over(
            app_name,
            Handover::Problem("The game is not installed any more.".into()),
        );
    };
    if game.folder_missing() {
        return hand_over(
            app_name,
            Handover::Problem(
                "The game folder cannot be found. Connect the drive it is on.".into(),
            ),
        );
    }
    // A second click on a shortcut, or a game started from the window, is
    // already being played; starting it twice helps nobody.
    if crate::running::alive()
        .iter()
        .any(|live| live.app_name == app_name)
    {
        info!("{} is already running", game.title);
        return 0;
    }

    let outcome = if cfg.tray.enabled {
        run_in_tray(lgd, cfg, game)
    } else {
        run(&lgd, &cfg, &game, false, &|_| {})
    };
    match outcome {
        Ok(outcome) if outcome.ok => 0,
        Ok(outcome) if outcome.out_of_date() => hand_over(app_name, Handover::Update),
        Ok(outcome) => {
            let reason = outcome
                .log_tail
                .last()
                .cloned()
                .unwrap_or_else(|| "legendary said nothing about why.".into());
            hand_over(app_name, Handover::Problem(reason))
        }
        Err(e) => {
            error!("cannot launch {app_name}: {e:#}");
            hand_over(app_name, Handover::Problem(format!("{e:#}")))
        }
    }
}

/// [`run`], with an icon in the tray for as long as the game is up.
///
/// The icon stays away while the window is open: the window has its own and
/// shows the game as running. It is the window's bus name that says so, which
/// comes and goes with it, so the icon follows the window both ways.
fn run_in_tray(lgd: Legendary, cfg: Config, game: Game) -> Result<LaunchOutcome> {
    let group = Arc::new(AtomicI32::new(0));
    let (done, finished) = async_channel::bounded(1);
    let title = game.title.clone();
    let started = Arc::clone(&group);
    thread::spawn(move || {
        let outcome = run(&lgd, &cfg, &game, false, &|leader| {
            started.store(leader, Ordering::SeqCst)
        });
        let _ = done.send_blocking(outcome);
    });

    let tray: Rc<RefCell<Option<Tray>>> = Rc::new(RefCell::new(None));
    let shown = Rc::clone(&tray);
    let watch = gio::bus_watch_name(
        gio::BusType::Session,
        crate::APP_ID,
        gio::BusNameWatcherFlags::NONE,
        {
            let tray = Rc::clone(&tray);
            move |_, _, _| *tray.borrow_mut() = None
        },
        move |_, _| {
            let group = Arc::clone(&group);
            let made = Tray::new(
                &title,
                vec![
                    Entry::Item("Open Divine".into()),
                    Entry::Item(format!("Stop {title}")),
                ],
                open_window,
                move |index| match index {
                    0 => open_window(),
                    _ => {
                        let leader = group.load(Ordering::SeqCst);
                        if leader != 0 {
                            crate::legendary::stop(leader, false);
                        }
                    }
                },
            );
            match made {
                Ok(made) => *shown.borrow_mut() = Some(made),
                Err(e) => warn!("no icon in the tray: {e:#}"),
            }
        },
    );

    let main_loop = glib::MainLoop::new(None, false);
    let result = Rc::new(RefCell::new(None));
    glib::spawn_future_local({
        let main_loop = main_loop.clone();
        let result = Rc::clone(&result);
        async move {
            *result.borrow_mut() = finished.recv().await.ok();
            main_loop.quit();
        }
    });
    main_loop.run();
    gio::bus_unwatch_name(watch);
    tray.borrow_mut().take();
    result
        .take()
        .unwrap_or_else(|| Err(anyhow::anyhow!("the game's thread went away")))
}

/// Starts the window beside the game. Waited on from a thread of its own, so
/// that it is not left behind as a zombie for as long as the game runs.
fn open_window() {
    let me = match std::env::current_exe() {
        Ok(me) => me,
        Err(e) => {
            warn!("cannot find Divine's own binary: {e:#}");
            return;
        }
    };
    match Command::new(me).spawn() {
        Ok(mut child) => {
            thread::spawn(move || child.wait());
        }
        Err(e) => warn!("cannot open the window: {e:#}"),
    }
}

/// Becomes the window, open on the game, with the question or the problem.
///
/// Replacing this process rather than starting another keeps it in whatever
/// launched the shortcut: under Steam, a game started after the update is
/// still Steam's. With a window already open, the new process hands its
/// command line over to that one and exits.
fn hand_over(app_name: &str, handover: Handover) -> i32 {
    let me = match std::env::current_exe() {
        Ok(me) => me,
        Err(e) => {
            error!("cannot find Divine's own binary to open the window: {e:#}");
            return 1;
        }
    };
    let error = Command::new(me)
        .arg("--game")
        .arg(app_name)
        .args(handover.args())
        .exec();
    warn!("cannot open the window: {error}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legendary_options_stay_apart_from_the_game_arguments() {
        let per_game = GameSettings {
            offline: true,
            skip_update_check: true,
            override_exe: "Binaries/Win64/game.exe".into(),
            language: "ru".into(),
            launch_options: "mangohud %command% -dx11 -windowed".into(),
            ..GameSettings::default()
        };
        let args = launch_args(&per_game, "").expect("building the arguments");
        assert_eq!(
            args.options,
            [
                "--offline",
                "--skip-version-check",
                "--override-exe",
                "Binaries/Win64/game.exe",
                "--language",
                "ru",
            ]
        );
        assert_eq!(args.game, ["-dx11", "-windowed"]);

        let none = launch_args(&GameSettings::default(), "").expect("building the arguments");
        assert!(none.options.is_empty() && none.game.is_empty());
    }

    #[test]
    fn a_game_name_beats_the_global_one() {
        let global = "Anonymous";
        let none = GameSettings::default();
        let options = |settings: &GameSettings, global: &str| {
            launch_args(settings, global)
                .expect("building the arguments")
                .options
        };
        assert_eq!(options(&none, global), ["--override-username", "Anonymous"]);

        let own = GameSettings {
            override_username: "Someone Else".into(),
            ..GameSettings::default()
        };
        assert_eq!(
            options(&own, global),
            ["--override-username", "Someone Else"]
        );
        // Neither set leaves the argument off, so legendary uses the real name.
        assert!(options(&none, "").is_empty());
    }
}
