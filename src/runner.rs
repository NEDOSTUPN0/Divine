//! Finding Proton builds on disk and turning one into a launch command.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::{self, Config, LATEST_GE_PROTON};
use crate::log::{error, warn};
use crate::models::Game;
use crate::umu;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    /// umu downloads and manages the latest GE-Proton itself.
    Managed,
    GeProton,
    CachyOs,
    /// Valve's own builds, shipped with Steam.
    Valve,
    Other,
}

#[derive(Debug, Clone)]
pub struct Runner {
    pub name: String,
    /// `None` for the umu-managed "latest GE-Proton" pseudo runner.
    pub path: Option<PathBuf>,
    pub kind: RunnerKind,
    /// Installed by Divine, so it is ours to delete again.
    pub managed: bool,
}

impl Runner {
    /// Valve's Proton works, but GE/CachyOS builds carry the media codecs and
    /// fixes most Epic titles need, so those are what we steer people towards.
    pub fn recommended(&self) -> bool {
        self.kind != RunnerKind::Valve
    }

    /// The value umu expects in PROTONPATH.
    pub fn proton_path(&self) -> String {
        match &self.path {
            Some(path) => path.display().to_string(),
            None => LATEST_GE_PROTON.to_string(),
        }
    }

    /// Where the build lives, so the list never implies Divine installed it.
    pub fn location(&self) -> String {
        match &self.path {
            Some(path) if self.managed => format!("Installed by Divine · {}", path.display()),
            Some(path) => path.display().to_string(),
            None => "Fetched and kept up to date by umu".to_string(),
        }
    }

    pub fn subtitle(&self) -> &'static str {
        match self.kind {
            RunnerKind::Managed => "Downloaded and patched up to date automatically",
            RunnerKind::GeProton => "GE-Proton",
            RunnerKind::CachyOs => "Proton CachyOS",
            RunnerKind::Valve => "Ships with Steam, not recommended",
            RunnerKind::Other => "Custom build",
        }
    }
}

/// Compatibility tool directories used by Steam, its Flatpak, umu and Divine.
fn tool_dirs(proton_dir: &Path) -> Vec<PathBuf> {
    let home = config::home();
    vec![
        proton_dir.to_path_buf(),
        home.join(".steam/root/compatibilitytools.d"),
        home.join(".steam/steam/compatibilitytools.d"),
        home.join(".local/share/Steam/compatibilitytools.d"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam/compatibilitytools.d"),
        home.join(".local/share/umu/compatibilitytools"),
    ]
}

/// Steam library roots, read out of libraryfolders.vdf without a full VDF parser.
fn steam_libraries() -> Vec<PathBuf> {
    let home = config::home();
    let candidates = [
        home.join(".steam/root/steamapps/libraryfolders.vdf"),
        home.join(".local/share/Steam/steamapps/libraryfolders.vdf"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam/steamapps/libraryfolders.vdf"),
    ];
    let mut libs = Vec::new();
    for vdf in candidates {
        let Ok(text) = fs::read_to_string(&vdf) else {
            continue;
        };
        for line in text.lines() {
            let parts: Vec<&str> = line.split('"').collect();
            // A path entry looks like:  "path"  "/Games/SteamLibrary"
            if parts.len() >= 5 && parts[1] == "path" {
                let path = PathBuf::from(parts[3]);
                if !libs.contains(&path) {
                    libs.push(path);
                }
            }
        }
    }
    libs
}

fn classify(dir: &Path, from_steam: bool) -> RunnerKind {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.contains("ge-proton") || name.contains("proton-ge") {
        RunnerKind::GeProton
    } else if name.contains("cachyos") {
        RunnerKind::CachyOs
    } else if from_steam {
        RunnerKind::Valve
    } else {
        RunnerKind::Other
    }
}

fn push_runner(runners: &mut Vec<Runner>, dir: PathBuf, from_steam: bool, managed: bool) {
    if !dir.join("proton").is_file() {
        return;
    }
    // Distro packaging loves symlinks, so dedupe on the resolved path.
    let real = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    if runners
        .iter()
        .any(|r| r.path.as_ref().is_some_and(|p| p == &real))
    {
        return;
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| real.display().to_string());
    runners.push(Runner {
        kind: classify(&dir, from_steam),
        name,
        path: Some(real),
        managed,
    });
}

pub fn discover(proton_dir: &Path) -> Vec<Runner> {
    let mut runners = vec![Runner {
        name: "GE-Proton (latest, via umu)".into(),
        path: None,
        kind: RunnerKind::Managed,
        managed: false,
    }];

    for dir in tool_dirs(proton_dir) {
        let ours = dir == proton_dir;
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            push_runner(&mut runners, entry.path(), false, ours);
        }
    }

    for lib in steam_libraries() {
        let common = lib.join("steamapps/common");
        let Ok(entries) = fs::read_dir(&common) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("Proton") {
                push_runner(&mut runners, entry.path(), true, false);
            }
        }
    }

    runners
}

/// Where the shim lands inside the prefix. Windows path, because that is what
/// gets handed to Proton.
const SHIM_WINDOWS_PATH: &str = r"C:\windows\command\EpicGamesLauncher.exe";

/// Copies the wrapper into the prefix so Proton can start it from a plain
/// Windows path, and hands back that path.
fn install_shim(drive_c: &Path) -> Result<String> {
    // Fetching here covers a wrapper that went missing after it was switched
    // on, and one a known fix switched on without the settings dialog ever
    // bringing it down.
    let shim = crate::shim::ensure().context("preparing the Epic Games Launcher wrapper")?;
    let dir = drive_c.join("windows").join("command");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    // fs::copy carries the source mode across, and a packaged install is
    // read-only (0444 in the Nix store), so the copy left behind by an earlier
    // launch cannot be written over. Drop it first, then make ours writable.
    let target = dir.join(crate::shim::EXE);
    if let Err(e) = fs::remove_file(&target) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e).with_context(|| format!("replacing {}", target.display()));
        }
    }
    fs::copy(&shim, &target).context("copying the Epic Games Launcher wrapper")?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644))
        .with_context(|| format!("setting permissions on {}", target.display()))?;

    Ok(SHIM_WINDOWS_PATH.to_string())
}

/// legendary runs the wrapper string through shlex, which eats backslashes and
/// splits on spaces, so every part goes in as a single quoted word.
fn quote(part: &str) -> String {
    format!("'{}'", part.replace('\'', r"'\''"))
}

/// The WINE prefix itself. umu keeps it in the directory as it is; Proton run
/// directly puts it one level down, in pfx.
pub fn prefix_path(game: &Game, cfg: &Config) -> PathBuf {
    let dir = cfg.general.prefixes_dir.join(prefix_name(game));
    let per_game = cfg.game(&game.app_name);
    if per_game.use_umu.unwrap_or(cfg.runner.use_umu) {
        dir
    } else {
        dir.join("pfx")
    }
}

/// Microsoft's own fonts. Epic's overlay hooks the game without them but draws
/// nothing at all, which is indistinguishable from a broken overlay.
pub fn has_core_fonts(prefix: &Path) -> bool {
    prefix.join("drive_c/windows/Fonts/times.ttf").is_file()
}

/// Prefix directory name for a game: readable, and free of shell surprises.
pub fn prefix_name(game: &Game) -> String {
    let cleaned: String = game
        .title
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if cleaned.is_empty() {
        game.app_name.clone()
    } else {
        cleaned
    }
}

/// Wrapper tools are the distribution's to provide, and a switch left on for one
/// that is not installed should not stop the game from starting.
fn require(binary: &str) -> bool {
    let found = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()));
    if !found {
        warn!("{binary} is switched on but not installed, skipping it");
    }
    found
}

/// The Proton build a game runs on: its own choice, or the global one.
fn proton_setting(cfg: &Config, per_game: &crate::config::GameSettings) -> String {
    if per_game.proton.is_empty() {
        cfg.runner.proton.clone()
    } else {
        per_game.proton.clone()
    }
}

/// The switches Proton reads out of the environment, worked out from the
/// settings alone. Split out because two of the rules are easy to get backwards
/// and neither is visible from the variable names.
fn tuning(cfg: &Config, per_game: &crate::config::GameSettings) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();

    // Proton turns all three on by itself; the environment only carries the
    // switches that turn them back off.
    for (enabled, key) in [
        (
            per_game.esync.unwrap_or(cfg.runner.esync),
            "PROTON_NO_ESYNC",
        ),
        (
            per_game.fsync.unwrap_or(cfg.runner.fsync),
            "PROTON_NO_FSYNC",
        ),
        (
            per_game.ntsync.unwrap_or(cfg.runner.ntsync),
            "PROTON_NO_NTSYNC",
        ),
    ] {
        if !enabled {
            env.push((key.into(), "1".into()));
        }
    }

    // Proton only acts on this when a Wayland session is actually there, and HDR
    // rides along with it: it goes through winewayland, so it is meaningless on
    // XWayland. Heroic ties the two together the same way.
    if per_game.wayland.unwrap_or(cfg.runner.wayland) {
        env.push(("PROTON_ENABLE_WAYLAND".into(), "1".into()));
        if per_game.hdr.unwrap_or(cfg.runner.hdr) {
            env.push(("PROTON_ENABLE_HDR".into(), "1".into()));
        }
    }

    env
}

/// The umu id a game runs under. An id the database does not know means no game
/// fix at all, so a hand-pinned one wins.
fn game_id(game: &Game, per_game: &crate::config::GameSettings) -> String {
    match per_game.umu_id.as_str() {
        "" => umu::game_id(&game.app_name).unwrap_or_else(|| umu::NEUTRAL_ID.to_string()),
        pinned => pinned.to_string(),
    }
}

/// Runs a winetricks verb in a game's prefix. umu takes winetricks from the
/// Proton build itself, so there is nothing extra to install. Blocking.
pub fn winetricks(game: &Game, cfg: &Config, verb: &str) -> Result<()> {
    let per_game = cfg.game(&game.app_name);
    let prefix = cfg.general.prefixes_dir.join(prefix_name(game));
    fs::create_dir_all(&prefix)?;

    let output = Command::new(&cfg.general.umu)
        .args(["winetricks", verb])
        // umu runs inside its own container, where a TMPDIR inherited from the
        // outside (a nix develop shell sets one) does not exist, and winetricks
        // dies on the first mktemp. Dropping it falls back to /tmp, which the
        // container does provide.
        .env_remove("TMPDIR")
        .env_remove("TMP")
        .env_remove("TEMP")
        .env("PROTONPATH", proton_setting(cfg, &per_game))
        .env("STORE", umu::STORE)
        .env("GAMEID", game_id(game, &per_game))
        .env("WINEPREFIX", &prefix)
        .output()
        .with_context(|| format!("running {} winetricks {verb}", cfg.general.umu))?;

    if !output.status.success() {
        let log = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
        bail!(
            "winetricks {verb} failed:\n{}",
            tail[tail.len().saturating_sub(10)..].join("\n")
        );
    }
    Ok(())
}

/// Turns Epic's overlay on or off in one prefix. Never fatal: a game whose
/// overlay cannot be set up should still start.
fn sync_eos_overlay(game: &Game, cfg: &Config, prefix: &Path, wanted: bool) {
    let lgd = crate::legendary::Legendary::new(cfg);
    // Nothing to switch off if the overlay was never downloaded in the first
    // place, which spares every launch a legendary call.
    if !wanted && !lgd.eos_overlay_installed() {
        return;
    }
    if wanted && !lgd.eos_overlay_installed() {
        if let Err(e) = lgd.eos_overlay_install(&config::eos_overlay_dir()) {
            error!("cannot install the EOS overlay: {e:#}");
            return;
        }
    }
    if !prefix.is_dir() {
        return;
    }
    if let Err(e) = lgd.eos_overlay_set(prefix, wanted) {
        error!("cannot change the EOS overlay: {e:#}");
        return;
    }
    // The overlay is useless without fonts, and the global switch has no other
    // moment to notice that this prefix has none.
    if wanted && !has_core_fonts(prefix) {
        if let Err(e) = winetricks(game, cfg, "corefonts") {
            error!("cannot install core fonts for the EOS overlay: {e:#}");
        }
    }
}

/// Builds the `--wrapper` string and the environment for `legendary launch`.
/// Blocking: resolving the umu game id may hit the network.
pub fn build_launch(game: &Game, cfg: &Config) -> Result<(String, Vec<(String, String)>)> {
    let per_game = cfg.game(&game.app_name);
    let prefix = cfg.general.prefixes_dir.join(prefix_name(game));
    fs::create_dir_all(&prefix)?;

    let proton_setting = proton_setting(cfg, &per_game);

    let mut wrapper: Vec<String> = Vec::new();
    if per_game.gamemode.unwrap_or(cfg.runner.gamemode) && require("gamemoderun") {
        wrapper.push("gamemoderun".into());
        // gamemoderun preloads libgamemodeauto and points LD_LIBRARY_PATH at the
        // client library it dlopens. umu's container replaces LD_LIBRARY_PATH,
        // so that lookup fails and GameMode never engages, quietly. Preloading
        // the client library too settles it: the dlopen matches an object that
        // is already loaded, by soname, without searching any path. This takes
        // over LD_PRELOAD, which is gamemoderun's to set anyway.
        wrapper.push("env".into());
        wrapper.push("LD_PRELOAD=libgamemodeauto.so.0:libgamemode.so.0".into());
    }
    if per_game.mangohud.unwrap_or(cfg.runner.mangohud) && require("mangohud") {
        wrapper.push("mangohud".into());
    }

    let mut env = tuning(cfg, &per_game);

    // Anti-cheat runtimes are per game, and Proton reads them straight from the
    // environment. Downloading here covers a runtime that went missing after it
    // was switched on, and one a known fix turned on that nobody has fetched:
    // the settings dialog only fetches what is switched on in front of it.
    let fix = crate::known_fixes::for_game(&game.app_name);
    for runtime in crate::runtimes::Runtime::ALL {
        if !per_game.runtime_enabled(runtime, fix.as_ref()) {
            continue;
        }
        crate::runtimes::ensure(runtime, &|_, _| {})
            .with_context(|| format!("preparing the {}", runtime.title()))?;
        env.push((
            runtime.env_var().into(),
            runtime.dir().display().to_string(),
        ));
    }

    let use_umu = per_game.use_umu.unwrap_or(cfg.runner.use_umu);
    let drive_c = if use_umu {
        prefix.join("drive_c")
    } else {
        prefix.join("pfx").join("drive_c")
    };

    if use_umu {
        wrapper.push(cfg.general.umu.clone());
        env.push(("PROTONPATH".into(), proton_setting.clone()));
        // The store decides which set of protonfixes umu consults.
        env.push(("STORE".into(), umu::STORE.into()));
        env.push(("GAMEID".into(), game_id(game, &per_game)));
        env.push(("WINEPREFIX".into(), prefix.display().to_string()));
    } else {
        if proton_setting == LATEST_GE_PROTON {
            bail!("Running without umu needs a specific Proton build, not the umu-managed latest");
        }
        let proton = PathBuf::from(&proton_setting).join("proton");
        if !proton.is_file() {
            bail!("{} not found", proton.display());
        }
        wrapper.push(proton.display().to_string());
        wrapper.push("run".into());
        // proton creates the prefix inside this directory, under pfx/.
        env.push((
            "STEAM_COMPAT_DATA_PATH".into(),
            prefix.display().to_string(),
        ));
        env.push((
            "STEAM_COMPAT_CLIENT_INSTALL_PATH".into(),
            config::home().join(".steam/root").display().to_string(),
        ));
    }

    // umu works this out from the executable, and pressure-vessel shows the
    // container nothing it was not handed: its own list stops at /home, /mnt,
    // /opt and /srv. With the Epic wrapper the executable umu sees is a Windows
    // path it cannot resolve, so it leaves this empty and a game installed
    // anywhere else is simply absent inside the container.
    if let Some(installed) = &game.installed {
        env.push((
            "STEAM_COMPAT_INSTALL_PATH".into(),
            installed.install_path.clone(),
        ));
    }

    // The overlay lives in the prefix's registry, so it is set right before the
    // game starts rather than carried in the environment.
    sync_eos_overlay(
        game,
        cfg,
        drive_c.parent().unwrap_or(&prefix),
        per_game.eos_overlay.unwrap_or(cfg.runner.eos_overlay),
    );

    // Goes last in the wrapper: legendary appends the game executable straight
    // after it, which is exactly the argument the shim expects.
    if per_game.fake_epic_launcher(fix.as_ref()) {
        wrapper.push(install_shim(&drive_c)?);
    }

    // Per-game variables go last so they can override anything set above.
    env.extend(per_game.env.iter().map(|(k, v)| (k.clone(), v.clone())));

    let wrapper = wrapper.iter().map(|part| quote(part)).collect::<Vec<_>>();
    Ok((wrapper.join(" "), env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GameSettings, General};

    fn game(app_name: &str, title: &str) -> Game {
        Game {
            app_name: app_name.into(),
            title: title.into(),
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
            folder: String::new(),
            partner: String::new(),
        }
    }

    fn config() -> Config {
        Config {
            general: General {
                prefixes_dir: PathBuf::from("/games/prefixes"),
                ..General::default()
            },
            ..Config::default()
        }
    }

    /// The wrapper is handed to legendary as one shell string, so a game living
    /// somewhere with an apostrophe in the name has to survive the trip.
    #[test]
    fn a_path_with_shell_characters_is_quoted_whole() {
        assert_eq!(quote("umu-run"), "'umu-run'");
        assert_eq!(
            quote("/games/Tom's Game/run"),
            r"'/games/Tom'\''s Game/run'"
        );
        // Nothing inside quotes needs escaping but the quote itself.
        assert_eq!(quote("a b$c;d"), "'a b$c;d'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn a_prefix_is_named_after_the_game_without_the_punctuation() {
        assert_eq!(
            prefix_name(&game("Fowl", "The Escapists 2")),
            "TheEscapists2"
        );
        assert_eq!(
            prefix_name(&game("Catnip", "Borderlands 3: Ultimate")),
            "Borderlands3Ultimate"
        );
        // A title with nothing ASCII left in it would name no directory at all,
        // so the app name stands in.
        assert_eq!(prefix_name(&game("Peony", "Атомное сердце")), "Peony");
    }

    /// umu owns the prefix directory; `proton run` makes its own pfx inside it.
    #[test]
    fn the_prefix_moves_down_a_level_without_umu() {
        let mut cfg = config();
        let game = game("Fowl", "The Escapists 2");

        cfg.runner.use_umu = true;
        assert_eq!(
            prefix_path(&game, &cfg),
            PathBuf::from("/games/prefixes/TheEscapists2")
        );

        cfg.runner.use_umu = false;
        assert_eq!(
            prefix_path(&game, &cfg),
            PathBuf::from("/games/prefixes/TheEscapists2/pfx")
        );

        // And one game may disagree with the global setting.
        cfg.set_game(
            "Fowl",
            GameSettings {
                use_umu: Some(true),
                ..GameSettings::default()
            },
        );
        assert_eq!(
            prefix_path(&game, &cfg),
            PathBuf::from("/games/prefixes/TheEscapists2")
        );
    }

    #[test]
    fn a_game_picks_its_own_proton_over_the_global_one() {
        let mut cfg = config();
        cfg.runner.proton = "/proton/GE-Proton10".into();
        assert_eq!(
            proton_setting(&cfg, &GameSettings::default()),
            "/proton/GE-Proton10"
        );

        let pinned = GameSettings {
            proton: "/proton/CachyOS".into(),
            ..GameSettings::default()
        };
        assert_eq!(proton_setting(&cfg, &pinned), "/proton/CachyOS");
    }

    /// The one rule worth pinning: these variables turn things *off*, so a
    /// setting that is on must leave nothing behind.
    #[test]
    fn the_sync_switches_are_only_written_when_turned_off() {
        let mut cfg = config();
        cfg.runner.esync = true;
        cfg.runner.fsync = true;
        cfg.runner.ntsync = true;
        assert!(tuning(&cfg, &GameSettings::default()).is_empty());

        cfg.runner.fsync = false;
        assert_eq!(
            tuning(&cfg, &GameSettings::default()),
            vec![("PROTON_NO_FSYNC".to_string(), "1".to_string())]
        );

        // A game that wants fsync back gets it, without touching the others.
        let per_game = GameSettings {
            fsync: Some(true),
            ..GameSettings::default()
        };
        assert!(tuning(&cfg, &per_game).is_empty());
    }

    /// HDR goes through winewayland, so on X11 the variable does nothing and
    /// writing it would only be misleading.
    #[test]
    fn hdr_never_travels_without_wayland() {
        let mut cfg = config();
        cfg.runner.esync = true;
        cfg.runner.fsync = true;
        cfg.runner.ntsync = true;
        cfg.runner.hdr = true;

        cfg.runner.wayland = false;
        assert!(tuning(&cfg, &GameSettings::default()).is_empty());

        cfg.runner.wayland = true;
        assert_eq!(
            tuning(&cfg, &GameSettings::default()),
            vec![
                ("PROTON_ENABLE_WAYLAND".to_string(), "1".to_string()),
                ("PROTON_ENABLE_HDR".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn a_pinned_umu_id_beats_the_database() {
        let per_game = GameSettings {
            umu_id: "umu-271590".into(),
            ..GameSettings::default()
        };
        // No lookup happens at all, which is the point: the database does not
        // know every game, and a hand-pinned id is the answer for those.
        assert_eq!(
            game_id(&game("Fowl", "The Escapists 2"), &per_game),
            "umu-271590"
        );
    }
}
