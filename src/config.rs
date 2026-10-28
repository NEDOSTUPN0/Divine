//! Divine's own settings plus the XDG paths it works with.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::log::warn;

const APP_DIR: &str = "divine";

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join(APP_DIR)
}

/// Where the log of a session lands. State rather than cache: it outlives the
/// run it describes, and it is the first thing to ask for once a launch has
/// gone wrong and the window is already closed.
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| home().join(".local/state"))
        .join(APP_DIR)
}

pub fn log_file() -> PathBuf {
    state_dir().join("divine.log")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| home().join(".cache"))
        .join(APP_DIR)
}

pub fn covers_dir() -> PathBuf {
    cache_dir().join("covers")
}

/// Epic's in-game overlay, downloaded once and shared by every prefix.
pub fn eos_overlay_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join(APP_DIR)
        .join("eos_overlay")
}

/// The Epic Games Launcher wrapper, when Divine had to fetch one because the
/// build did not ship it. A packaged copy under share/divine wins over this.
pub fn shim_file() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join(APP_DIR)
        .join(crate::shim::EXE)
}

/// Anti-cheat runtimes Divine downloads, one directory per runtime.
pub fn runtimes_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join(APP_DIR)
        .join("runtimes")
}

/// Divine keeps legendary's state to itself so a system-wide legendary install
/// (whose config.ini may even be read-only) is never touched. Heroic does the same.
/// Cached manifest sizes, one file per game build.
pub fn manifests_dir() -> PathBuf {
    cache_dir().join("manifests")
}

/// Cached store text, one file per game namespace.
pub fn store_dir() -> PathBuf {
    cache_dir().join("store")
}

/// Cached component lists, one file per game that has them.
pub fn sdl_dir() -> PathBuf {
    cache_dir().join("sdl")
}

pub fn legendary_config_dir() -> PathBuf {
    config_dir().join("legendaryConfig").join("legendary")
}

/// legendary/heroic config directories we can import already-installed games from.
pub fn foreign_legendary_dirs() -> Vec<PathBuf> {
    let base = dirs::config_dir().unwrap_or_else(|| home().join(".config"));
    vec![
        base.join("legendary"),
        base.join("heroic")
            .join("legendaryConfig")
            .join("legendary"),
    ]
}

/// How big the window was left, so it comes back the same size.
///
/// Only what the window manager cannot work out for itself: the position is
/// none of an application's business under Wayland, and asking for one is
/// ignored anyway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1150,
            height: 760,
            maximized: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub legendary: String,
    pub umu: String,
    pub install_dir: PathBuf,
    pub prefixes_dir: PathBuf,
    /// Where builds downloaded through Divine are unpacked.
    pub proton_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RunnerSettings {
    /// umu handles the prefix, protonfixes and the Steam Linux Runtime. With it
    /// off, Divine calls `proton run` directly, which is really only for
    /// debugging umu itself.
    pub use_umu: bool,
    /// Absolute path to a Proton directory, or the literal "GE-Proton" which
    /// tells umu to fetch and use the latest GE-Proton release.
    pub proton: String,
    pub gamemode: bool,
    pub mangohud: bool,
    /// Not alternatives: Proton turns all three on and Wine picks the best the
    /// kernel offers, so the environment only carries switches to turn them off.
    /// ntsync exists in GE-Proton and CachyOS builds only, and Valve's Proton
    /// ignores the variable.
    pub esync: bool,
    pub fsync: bool,
    pub ntsync: bool,
    /// Swap winex11.drv for winewayland.drv. GE-Proton and CachyOS builds only,
    /// and still experimental upstream.
    pub wayland: bool,
    /// Only reaches the game together with `wayland`: Proton drives HDR through
    /// winewayland, so on XWayland the variable does nothing. Heroic pairs them
    /// the same way.
    pub hdr: bool,
    /// Epic's in-game overlay, for the games that use Epic Online Services.
    pub eos_overlay: bool,
    /// Sync saves with Epic around every launch, for the games that support it.
    pub cloud_saves: bool,
    /// Name handed to games instead of the Epic display name. Empty leaves it
    /// alone. Only the one argument changes: the account id and the login code
    /// stay real, so this is a label, not a disguise.
    pub override_username: String,
    /// Report finished play sessions to Epic, so its counter matches Divine's.
    pub report_playtime: bool,
    /// Valve's own builds are hidden until asked for.
    pub show_steam_proton: bool,
}

/// Per-game overrides, keyed by Epic app name. Every field is optional: an empty
/// string means "use the global setting".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    /// umu game id, e.g. "umu-271590". Empty means Divine looks it up.
    pub umu_id: String,
    pub proton: String,
    pub args: String,
    /// Two letter language code handed to legendary, e.g. "ru".
    pub language: String,
    /// Executable to start instead of the one the manifest names, relative to
    /// the game folder. legendary's own answer to a game that ships a launcher
    /// of its own, which is the case its warnings point at.
    pub override_exe: String,
    /// Overrides the global name games are told. Empty follows it.
    pub override_username: String,
    /// Start without asking Epic anything. Skips the update check too, since
    /// legendary only makes that while online.
    pub offline: bool,
    /// Start a build Epic has moved past. Without this legendary refuses: it
    /// exits rather than warning.
    pub skip_update_check: bool,
    /// Start the game through a wrapper named EpicGamesLauncher.exe. Rockstar
    /// titles refuse to see the game as owned without it.
    pub use_fake_epic_exe: bool,
    /// Anti-cheat runtimes Proton mounts into the prefix. Per game only: they
    /// are dead weight for the many titles that ship neither anti-cheat.
    pub eac_runtime: bool,
    pub battleye_runtime: bool,
    /// Overrides of the matching global switches. `None` means "follow global";
    /// they are skipped when writing so an untouched game stays out of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_umu: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wayland: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eos_overlay: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_saves: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esync: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fsync: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ntsync: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamemode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mangohud: Option<bool>,
    /// Extra environment for the game. Kept last: TOML wants tables after values.
    pub env: BTreeMap<String, String>,
}

impl GameSettings {
    pub fn runtime_enabled(&self, runtime: crate::runtimes::Runtime) -> bool {
        match runtime {
            crate::runtimes::Runtime::Eac => self.eac_runtime,
            crate::runtimes::Runtime::BattlEye => self.battleye_runtime,
        }
    }

    /// True when nothing has been overridden, so the entry can be dropped.
    fn is_default(&self) -> bool {
        self.umu_id.is_empty()
            && self.proton.is_empty()
            && self.args.is_empty()
            && self.language.is_empty()
            && self.override_exe.is_empty()
            && self.override_username.is_empty()
            && !self.offline
            && !self.skip_update_check
            && !self.use_fake_epic_exe
            && !self.eac_runtime
            && !self.battleye_runtime
            && self.env.is_empty()
            && [
                self.use_umu,
                self.wayland,
                self.hdr,
                self.eos_overlay,
                self.cloud_saves,
                self.esync,
                self.fsync,
                self.ntsync,
                self.gamemode,
                self.mangohud,
            ]
            .iter()
            .all(Option::is_none)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    /// Ceiling for downloads in KiB/s, zero for none. legendary has no
    /// bandwidth setting, so Divine proxies its downloads to hold this; see
    /// src/throttle.rs. One ceiling covers every download at once, which is
    /// the only reading that makes sense with several of them running.
    pub limit: u64,
    /// Name a new install's folder the way Discord expects; see src/discord.rs.
    pub discord_folders: bool,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            limit: 0,
            discord_folders: true,
        }
    }
}

impl DownloadSettings {
    pub fn limit_bytes(&self) -> u64 {
        self.limit.saturating_mul(1024)
    }
}

/// How a trailer on a game page sounds.
///
/// Silent to begin with, because a page that starts talking the moment it is
/// opened is a nasty surprise, and because the library is browsed a page at a
/// time. Whoever turns the sound up is only asked once.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TrailerSettings {
    pub muted: bool,
    /// Between 0 and 1, as GStreamer counts it.
    pub volume: f64,
}

impl Default for TrailerSettings {
    fn default() -> Self {
        Self {
            muted: true,
            volume: 1.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub window: WindowState,
    pub runner: RunnerSettings,
    pub downloads: DownloadSettings,
    pub trailers: TrailerSettings,
    pub games: BTreeMap<String, GameSettings>,
}

impl Config {
    pub fn game(&self, app_name: &str) -> GameSettings {
        self.games.get(app_name).cloned().unwrap_or_default()
    }

    pub fn set_game(&mut self, app_name: &str, settings: GameSettings) {
        if settings.is_default() {
            self.games.remove(app_name);
        } else {
            self.games.insert(app_name.to_string(), settings);
        }
    }
}

/// What umu takes to mean "the latest GE-Proton, and keep it that way".
///
/// `GE-Latest` rather than `GE-Proton`: both fetch the newest release, but only
/// this one is updated by binary patch into a single directory (`umu_proton.py`
/// skips the patch path for anything that is not a `*-Latest` token). The other
/// downloads a fresh 1.5 GB build per release and leaves every old one on disk.
pub const LATEST_GE_PROTON: &str = "GE-Latest";

/// What the same thing used to be called here. A config written before the
/// change still works — umu understands both — but it would download whole
/// builds forever, and the Proton page would have nothing to select.
const LATEST_GE_PROTON_WAS: &str = "GE-Proton";

impl Default for General {
    fn default() -> Self {
        Self {
            legendary: "legendary".into(),
            umu: "umu-run".into(),
            install_dir: home().join("Games").join("Divine"),
            prefixes_dir: home().join("Games").join("Prefixes"),
            proton_dir: default_proton_dir(),
        }
    }
}

/// Divine's own tool directory by default: `~/.steam/root/compatibilitytools.d`
/// is often managed by the distro (on NixOS it holds read-only store symlinks),
/// so we stay out of it unless the user points us there.
pub fn default_proton_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join(APP_DIR)
        .join("compatibilitytools")
}

impl Default for RunnerSettings {
    fn default() -> Self {
        Self {
            use_umu: true,
            proton: LATEST_GE_PROTON.into(),
            gamemode: false,
            mangohud: false,
            esync: true,
            fsync: true,
            ntsync: true,
            wayland: false,
            hdr: false,
            eos_overlay: false,
            cloud_saves: true,
            override_username: String::new(),
            report_playtime: true,
            show_steam_proton: false,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// Never fails: a missing or broken config falls back to defaults.
    pub fn load() -> Self {
        let mut cfg: Self = fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| match toml::from_str(&s) {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    warn!("ignoring broken config.toml: {e:#}");
                    None
                }
            })
            .unwrap_or_default();

        if cfg.runner.proton == LATEST_GE_PROTON_WAS {
            cfg.runner.proton = LATEST_GE_PROTON.into();
        }
        for settings in cfg.games.values_mut() {
            if settings.proton == LATEST_GE_PROTON_WAS {
                settings.proton = LATEST_GE_PROTON.into();
            }
        }
        cfg
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(config_dir())?;
        let text = toml::to_string_pretty(self)?;
        fs::write(Self::path(), text).context("writing config.toml")
    }
}

/// Creates the directories Divine owns and seeds legendary's config.ini once.
pub fn ensure_dirs() -> Result<()> {
    fs::create_dir_all(config_dir())?;
    fs::create_dir_all(covers_dir())?;
    fs::create_dir_all(state_dir())?;
    let lgd = legendary_config_dir();
    fs::create_dir_all(&lgd)?;

    let ini = lgd.join("config.ini");
    if !ini.exists() {
        // Only the update nagging; everything else is passed per invocation so
        // the config file can never silently override what the UI shows.
        fs::write(
            &ini,
            "[Legendary]\ndisable_update_check = true\ndisable_update_notice = true\n",
        )
        .context("seeding legendary config.ini")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings that say nothing are not written down. Otherwise every game
    /// ever opened would leave an empty table behind in config.toml.
    #[test]
    fn a_game_with_nothing_to_say_is_not_kept() {
        let mut cfg = Config::default();
        cfg.set_game("Fowl", GameSettings::default());
        assert!(cfg.games.is_empty());

        cfg.set_game(
            "Fowl",
            GameSettings {
                args: "-windowed".into(),
                ..GameSettings::default()
            },
        );
        assert_eq!(cfg.games.len(), 1);

        // And clearing the last override takes the entry away again.
        cfg.set_game("Fowl", GameSettings::default());
        assert!(cfg.games.is_empty());
    }

    #[test]
    fn an_unknown_game_reads_as_the_defaults() {
        let cfg = Config::default();
        let settings = cfg.game("never-seen");
        assert!(settings.proton.is_empty());
        // None, not false: "no opinion" has to stay distinguishable from "off",
        // or every game would silently override the global setting.
        assert_eq!(settings.use_umu, None);
        assert_eq!(settings.hdr, None);
    }

    /// The round trip config.toml actually makes on disk.
    #[test]
    fn settings_survive_being_written_and_read_back() {
        let mut cfg = Config::default();
        cfg.runner.proton = "/proton/GE-Proton10".into();
        cfg.downloads.limit = 2048;
        cfg.window = WindowState {
            width: 1280,
            height: 900,
            maximized: false,
        };
        cfg.set_game(
            "Fowl",
            GameSettings {
                args: "-windowed -nosplash".into(),
                hdr: Some(false),
                env: [("DXVK_HUD".to_string(), "fps".to_string())]
                    .into_iter()
                    .collect(),
                ..GameSettings::default()
            },
        );

        let text = toml::to_string(&cfg).expect("writing config.toml");
        let read: Config = toml::from_str(&text).expect("reading config.toml");

        assert_eq!(read.runner.proton, "/proton/GE-Proton10");
        assert_eq!(read.downloads.limit, 2048);
        assert_eq!((read.window.width, read.window.height), (1280, 900));
        let fowl = read.game("Fowl");
        assert_eq!(fowl.args, "-windowed -nosplash");
        assert_eq!(fowl.hdr, Some(false));
        assert_eq!(fowl.env.get("DXVK_HUD").map(String::as_str), Some("fps"));
    }

    /// A config written by an older build knows nothing about settings added
    /// since, and must still load with the rest of it intact.
    #[test]
    fn a_config_missing_whole_sections_still_loads() {
        let cfg: Config = toml::from_str("[runner]\nproton = \"/proton/CachyOS\"\n")
            .expect("reading a partial config.toml");
        assert_eq!(cfg.runner.proton, "/proton/CachyOS");
        assert_eq!(cfg.downloads.limit, DownloadSettings::default().limit);
        assert!(cfg.games.is_empty());
        // A size nobody wrote down is the default one, never zero: a window
        // opened at 0 by 0 is a window nobody can get hold of.
        assert_eq!(cfg.window.width, WindowState::default().width);
        assert_eq!(cfg.window.height, WindowState::default().height);

        // Empty is the extreme of the same case.
        assert!(toml::from_str::<Config>("").is_ok());
    }

    #[test]
    fn the_download_limit_is_written_in_kib_and_used_in_bytes() {
        let limit_bytes = |limit| {
            DownloadSettings {
                limit,
                ..Default::default()
            }
            .limit_bytes()
        };
        assert_eq!(limit_bytes(0), 0);
        assert_eq!(limit_bytes(1024), 1024 * 1024);
        // Nonsense from a hand-edited file must not wrap around to a tiny limit.
        assert_eq!(limit_bytes(u64::MAX), u64::MAX);
    }
}
