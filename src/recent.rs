//! The games started most recently, newest first, for the tray to offer.
//!
//! Kept by Divine because nothing else keeps it: the playtime journal empties
//! as Epic takes each session, and Epic's own record is a network call away.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// More than the tray shows, so that games uninstalled since still leave it
/// enough to fill its list.
const KEPT: usize = 20;

/// Writes down that a game has just been started.
pub fn played(app_name: &str) -> Result<()> {
    record(&file(), app_name)
}

/// App names, newest first.
pub fn list() -> Vec<String> {
    read(&file())
}

fn record(file: &Path, app_name: &str) -> Result<()> {
    let mut recent = read(file);
    recent.retain(|other| other != app_name);
    recent.insert(0, app_name.to_string());
    recent.truncate(KEPT);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // The window and a shortcut launch may both be writing.
    let partial = file.with_extension(format!("{}.partial", std::process::id()));
    fs::write(&partial, serde_json::to_string(&recent)?)
        .with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, file).with_context(|| format!("writing {}", file.display()))
}

fn read(file: &Path) -> Vec<String> {
    fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn file() -> PathBuf {
    crate::config::state_dir().join("recent.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_played_again_moves_to_the_front_once() {
        let file = std::env::temp_dir().join(format!("divine-recent-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        for game in ["Fowl", "Sugar", "Fowl"] {
            record(&file, game).unwrap();
        }
        assert_eq!(read(&file), ["Fowl", "Sugar"]);
        fs::remove_file(&file).unwrap();
    }

    #[test]
    fn only_so_many_are_kept() {
        let file =
            std::env::temp_dir().join(format!("divine-recent-many-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        for index in 0..KEPT + 5 {
            record(&file, &format!("game{index}")).unwrap();
        }
        let recent = read(&file);
        assert_eq!(recent.len(), KEPT);
        assert_eq!(recent[0], format!("game{}", KEPT + 4));
        fs::remove_file(&file).unwrap();
    }
}
