//! The Windows programs a game runs, learned by watching it.
//!
//! Under winewayland a window is named after the program that opened it, and
//! that is often not the one legendary starts: Grand Theft Auto V starts
//! `PlayGTAV.exe`, which starts the Rockstar launcher, which starts the game.
//! Nobody can list those programs in advance, so while a game runs its
//! processes are looked at every few seconds and every program of its own is
//! written down, for the menu to give each one a hidden entry of its own.
//!
//! A game's processes are found by `DIVINE_APP_NAME` in their environment,
//! which Divine sets on the launch and every program inherits. Neither the
//! process group nor the parent chain would do: pressure-vessel starts a group
//! of its own for the game, and a program whose parent has exited is handed to
//! whoever adopts orphans.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::log::warn;

/// The variable that marks a process as one of a game's.
pub const MARK: &str = "DIVINE_APP_NAME";
/// How often a running game is looked at. A window opened in between is
/// matched as soon as its entry exists, so this is how long an icon can lag.
const EVERY: Duration = Duration::from_secs(5);

/// One program of a game's, by the name its window will carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// The file name in lower case, which is the window's app id.
    pub class: String,
    pub path: PathBuf,
}

/// Every program a game has been seen to run, the one legendary starts aside.
pub fn known(app_name: &str) -> Vec<Program> {
    load(&store_file()).remove(app_name).unwrap_or_default()
}

/// Watches a game until dropped, calling `found` from its own thread each
/// time a program turns up that was not known before.
pub struct Watch {
    stop: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Watch {
    pub fn start(
        app_name: &str,
        prefix: &Path,
        install_dir: &Path,
        found: impl Fn() + Send + 'static,
    ) -> Self {
        let (stop, halt) = mpsc::channel();
        let app_name = app_name.to_string();
        let prefix = prefix.to_path_buf();
        let install_dir = install_dir.to_path_buf();
        let thread = thread::spawn(move || {
            while matches!(
                halt.recv_timeout(EVERY),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                let seen = running(&app_name, &prefix, &install_dir);
                match remember(&store_file(), &app_name, seen) {
                    Ok(true) => found(),
                    Ok(false) => {}
                    Err(e) => warn!("cannot keep the programs {app_name} runs: {e:#}"),
                }
            }
        });
        Watch {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The game's own programs running right now.
fn running(app_name: &str, prefix: &Path, install_dir: &Path) -> Vec<Program> {
    let mark = format!("{MARK}={app_name}");
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let dir = entry.path();
        // Somebody else's processes, and those gone since the listing, cannot
        // be read, and are not the game's anyway.
        let Ok(environ) = fs::read(dir.join("environ")) else {
            continue;
        };
        if !environ
            .split(|byte| *byte == 0)
            .any(|var| var == mark.as_bytes())
        {
            continue;
        }
        let Ok(cmdline) = fs::read(dir.join("cmdline")) else {
            continue;
        };
        let Some(argv0) = cmdline.split(|byte| *byte == 0).next() else {
            continue;
        };
        if let Some(program) = own_program(&String::from_utf8_lossy(argv0), prefix, install_dir) {
            if !found.contains(&program) {
                found.push(program);
            }
        }
    }
    found
}

/// The program a Wine process runs, if it is one of the game's own: inside
/// the game's folder, or installed into the prefix by the game. Wine's and
/// Proton's own programs are neither.
fn own_program(argv0: &str, prefix: &Path, install_dir: &Path) -> Option<Program> {
    let path = unix_path(argv0, prefix)?;
    let file = path.file_name()?.to_string_lossy().to_lowercase();
    if !file.ends_with(".exe") {
        return None;
    }
    let drive_c = prefix.join("drive_c");
    let in_prefix = path.starts_with(&drive_c) && !path.starts_with(drive_c.join("windows"));
    if !in_prefix && !path.starts_with(install_dir) {
        return None;
    }
    Some(Program { class: file, path })
}

/// A Windows path as Wine shows it in a process's command line, such as
/// `C:\Program Files\Rockstar Games\Launcher\Launcher.exe`, made into the file
/// on this machine through the prefix's drive letters.
fn unix_path(windows: &str, prefix: &Path) -> Option<PathBuf> {
    let windows = windows.strip_prefix(r"\\?\").unwrap_or(windows);
    let (drive, rest) = windows.split_once(":\\")?;
    let [letter] = drive.as_bytes() else {
        return None;
    };
    let devices = prefix.join("dosdevices");
    let link = devices.join(format!("{}:", letter.to_ascii_lowercase() as char));
    let target = fs::read_link(&link).ok()?;
    let root = if target.is_absolute() {
        target
    } else {
        devices.join(target)
    };
    let mut path = root;
    // Wine joins some paths of its own with forward slashes.
    for part in rest.split(['\\', '/']).filter(|part| !part.is_empty()) {
        path.push(part);
    }
    // ../drive_c in the link, said once so that the folder checks compare
    // like with like.
    Some(normalise(&path))
}

/// Drops `.` and folds `..` without asking the filesystem, which may name a
/// drive that is not plugged in.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Adds what was seen to what was known. True when anything was new.
fn remember(file: &Path, app_name: &str, seen: Vec<Program>) -> Result<bool> {
    if seen.is_empty() {
        return Ok(false);
    }
    let mut all = load(file);
    let known = all.entry(app_name.to_string()).or_default();
    let mut new = false;
    for program in seen {
        match known.iter_mut().find(|old| old.class == program.class) {
            // Moved with the game, or reinstalled elsewhere.
            Some(old) if old.path != program.path => {
                old.path = program.path;
                new = true;
            }
            Some(_) => {}
            None => {
                known.push(program);
                new = true;
            }
        }
    }
    if new {
        save(file, &all)?;
    }
    Ok(new)
}

fn load(file: &Path) -> BTreeMap<String, Vec<Program>> {
    fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(file: &Path, all: &BTreeMap<String, Vec<Program>>) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let partial = file.with_extension(format!("{}.partial", std::process::id()));
    fs::write(&partial, serde_json::to_string_pretty(all)?)
        .with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, file).with_context(|| format!("writing {}", file.display()))
}

fn store_file() -> PathBuf {
    crate::config::cache_dir().join("programs.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prefix laid out as umu leaves one, with the drive letters it makes.
    fn prefix(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("divine-programs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("dosdevices")).unwrap();
        fs::create_dir_all(dir.join("drive_c")).unwrap();
        std::os::unix::fs::symlink("../drive_c", dir.join("dosdevices/c:")).unwrap();
        std::os::unix::fs::symlink("/", dir.join("dosdevices/z:")).unwrap();
        dir
    }

    /// The command lines as they read in /proc while GTA V runs and while
    /// Notepad did in a test prefix.
    #[test]
    fn only_the_games_own_programs_are_kept() {
        let pfx = prefix("own");
        let game = Path::new("/Games/Grand Theft Auto V Enhanced");
        let own = |argv0| own_program(argv0, &pfx, game).map(|program| program.class);

        assert_eq!(
            own(r"Z:\Games\Grand Theft Auto V Enhanced\GTA5_Enhanced.exe").as_deref(),
            Some("gta5_enhanced.exe")
        );
        assert_eq!(
            own(r"C:\Program Files\Rockstar Games\Launcher\Launcher.exe").as_deref(),
            Some("launcher.exe")
        );
        // Wine's own, Proton's own, and the Epic wrapper Divine puts in.
        assert_eq!(own(r"C:\windows\system32\services.exe"), None);
        assert_eq!(own(r"C:\windows\command\EpicGamesLauncher.exe"), None);
        assert_eq!(
            own(r"\\?\Z:\Games\Protons\GE-Proton11-12\files\share\wine/../xalia/xalia.exe"),
            None
        );
        // Not a Windows program at all.
        assert_eq!(own("python3"), None);
        fs::remove_dir_all(&pfx).unwrap();
    }

    #[test]
    fn a_drive_letter_leads_where_the_prefix_says() {
        let pfx = prefix("drive");
        assert_eq!(
            unix_path(
                r"C:\Program Files\Rockstar Games\Launcher\Launcher.exe",
                &pfx
            ),
            Some(pfx.join("drive_c/Program Files/Rockstar Games/Launcher/Launcher.exe"))
        );
        assert_eq!(
            unix_path(r"Z:\Games\Divine\TheEscapists2\TheEscapists2.exe", &pfx),
            Some(PathBuf::from(
                "/Games/Divine/TheEscapists2/TheEscapists2.exe"
            ))
        );
        // A letter the prefix does not have.
        assert_eq!(unix_path(r"Q:\game.exe", &pfx), None);
        fs::remove_dir_all(&pfx).unwrap();
    }

    #[test]
    fn a_program_is_news_once() {
        let file =
            std::env::temp_dir().join(format!("divine-programs-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let launcher = Program {
            class: "launcher.exe".into(),
            path: "/pfx/drive_c/Launcher.exe".into(),
        };
        assert!(remember(&file, "gta", vec![launcher.clone()]).unwrap());
        assert!(!remember(&file, "gta", vec![launcher.clone()]).unwrap());
        // The same program somewhere else is news again.
        let moved = Program {
            path: "/elsewhere/Launcher.exe".into(),
            ..launcher
        };
        assert!(remember(&file, "gta", vec![moved.clone()]).unwrap());
        assert_eq!(load(&file)["gta"], [moved]);
        fs::remove_file(&file).unwrap();
    }
}
