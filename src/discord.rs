//! Install folders that Discord recognises.
//!
//! Discord tells which game is running by the tail of its executable's path,
//! folder included: it knows Grand Theft Auto V Enhanced as
//! `grand theft auto v enhanced/gta5_enhanced.exe` and by nothing else. Most of
//! those paths are Steam's install folders, and Epic names its folders its own
//! way (`GTAVEnhanced`), so a game installed where legendary would put it can
//! run without Discord ever noticing. Under Proton the path Discord reads is
//! the Windows one, `S:\GTAVEnhanced\GTA5_Enhanced.exe`, so only the folder's
//! name matters and not the disk it is on.
//!
//! The list comes from the endpoint Discord serves it on, which is not a
//! documented API. Anything short of a clear answer leaves Epic's name alone.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::http;
use crate::legendary::Legendary;
use crate::log::{debug, info, warn};

const DETECTABLE: &str = "https://discord.com/api/v9/applications/detectable";

/// The answer runs to about 13 MB, past ureq's default ceiling of 10.
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// A download rather than a question, but one an install is waiting on.
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct App {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    executables: Vec<Executable>,
}

#[derive(Deserialize)]
struct Executable {
    name: String,
    #[serde(default)]
    os: String,
}

/// The folder to install a game into so that Discord recognises it, or `None`
/// to keep Epic's: when Discord's list cannot be had, has never heard of the
/// game, or would accept Epic's name anyway. Blocking.
pub fn install_folder(lgd: &Legendary, app_name: &str, title: &str, epic: &str) -> Option<String> {
    let apps = match detectable() {
        Ok(apps) => apps,
        Err(e) => {
            warn!("keeping Epic's folder name for {title}: {e:#}");
            return None;
        }
    };
    let files = match lgd.files(app_name) {
        Ok(files) => files,
        Err(e) => {
            warn!("keeping Epic's folder name for {title}: {e:#}");
            return None;
        }
    };
    let folder = pick(&apps, title, &files, epic)?;
    info!("installing {title} into \"{folder}\", the folder Discord knows it by");
    Some(folder)
}

/// Discord's list, fetched once a session. A failure is not remembered:
/// installs are rare, and the next one may find Discord answering.
fn detectable() -> Result<Arc<Vec<App>>> {
    static LIST: Mutex<Option<Arc<Vec<App>>>> = Mutex::new(None);
    if let Some(list) = LIST.lock().ok().and_then(|held| held.clone()) {
        return Ok(list);
    }

    // Not holding the lock across the download, for the same reason as
    // http::remembered: asking twice costs less than waiting on each other.
    debug!("asking {DETECTABLE}");
    let body = http::agent(TIMEOUT)
        .get(DETECTABLE)
        .call()
        .context("asking Discord for the games it detects")?
        .into_body()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .context("reading Discord's list of games")?;
    let apps: Vec<App> = serde_json::from_str(&body).context("parsing Discord's list of games")?;
    let list = Arc::new(apps);
    if let Ok(mut held) = LIST.lock() {
        *held = Some(list.clone());
    }
    Ok(list)
}

/// Matches the game on two counts at once: by title, against the name Discord
/// gives it or one of its aliases, and by a file its manifest actually ships.
/// The title alone would mistake one game for another of the same name; the
/// file alone would match the `launcher.exe` every other game ships.
fn pick(apps: &[App], title: &str, files: &[String], epic: &str) -> Option<String> {
    let wanted = squash(title);
    let files: Vec<String> = files
        .iter()
        .map(|file| file.replace('\\', "/").to_lowercase())
        .collect();

    let mut folders: Vec<&str> = Vec::new();
    let same_game = apps.iter().filter(|app| {
        squash(&app.name) == wanted || app.aliases.iter().any(|alias| squash(alias) == wanted)
    });
    for app in same_game {
        for exe in app.executables.iter().filter(|exe| exe.os == "win32") {
            for file in &files {
                let Some(rest) = exe.name.strip_suffix(file.as_str()) else {
                    continue;
                };
                // An entry with no folder in it matches wherever the game is.
                if rest.is_empty() {
                    return None;
                }
                if let Some(folder) = rest.strip_suffix('/').filter(|f| usable(f)) {
                    folders.push(folder);
                }
            }
        }
    }

    let epic = epic.to_lowercase();
    if folders.is_empty() || folders.iter().any(|folder| *folder == epic) {
        return None;
    }
    // Discord only ever compares in lower case, so the title's own spelling
    // is kept whenever it is the folder Discord expects.
    let lowered = title.to_lowercase();
    if folders.iter().any(|folder| *folder == lowered) {
        return Some(title.to_string());
    }
    folders.first().map(|folder| folder.to_string())
}

/// Letters and digits only, so "Rocket League®" and "Rocket League" agree.
fn squash(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// One folder name Windows would accept, so the path legendary builds from it
/// stays inside the install directory and Wine can still open it.
fn usable(folder: &str) -> bool {
    !folder.is_empty()
        && folder != "."
        && folder != ".."
        && !folder.ends_with([' ', '.'])
        && !folder
            .chars()
            .any(|c| c.is_control() || "/\\<>:\"|?*".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from Discord's live answer.
    fn apps() -> Vec<App> {
        serde_json::from_str(
            r#"[
            {"aliases": [], "executables": [
                {"is_launcher": false, "name": "grand theft auto v enhanced/gta5_enhanced.exe", "os": "win32"},
                {"is_launcher": false, "name": "grand theft auto v enhanced/gta5_enhanced_be.exe", "os": "win32"}],
             "id": "1421192030440263791", "name": "Grand Theft Auto V Enhanced"},
            {"aliases": ["Grand Theft Auto V", "Grand Theft Auto V Enhanced", "Grand Theft Auto V Legacy"],
             "executables": [
                {"is_launcher": false, "name": "subprocess/fivem_gtaprocess.exe", "os": "win32"},
                {"is_launcher": false, "name": "lspdfr/lspdfr configurator.exe", "os": "win32"},
                {"is_launcher": false, "name": "gta5.exe", "os": "win32"},
                {"is_launcher": false, "name": "grand theft auto v/gta5_be.exe", "os": "win32"},
                {"is_launcher": false, "name": "grand theft auto v/fivem.exe", "os": "win32"},
                {"is_launcher": false, "name": "grand theft auto v/gta5.exe", "os": "win32"},
                {"is_launcher": false, "name": "grand theft auto v/ragepluginhook.exe", "os": "win32"},
                {"is_launcher": false, "name": "installers/social-club-setup.exe", "os": "win32"}],
             "id": "1402418714716143646", "name": "Grand Theft Auto V Legacy"},
            {"aliases": ["Among Us"], "executables": [
                {"is_launcher": false, "name": "among us - copy/among us.exe", "os": "win32"},
                {"is_launcher": false, "name": "among us - kopie/among us.exe", "os": "win32"},
                {"is_launcher": false, "name": "among us - tou/among us.exe", "os": "win32"},
                {"is_launcher": false, "name": "among us/among us.exe", "os": "win32"}],
             "id": "1402418440685486130", "name": "Among Us"},
            {"aliases": ["Rocket League"], "executables": [
                {"is_launcher": false, "name": "rocketleague.exe", "os": "win32"}],
             "id": "356877880938070016", "name": "Rocket League"}
        ]"#,
        )
        .expect("parsing the trimmed answer")
    }

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn gta_goes_where_steam_would_have_put_it() {
        // Every executable `legendary list-files` gives for the Epic release.
        // Legacy's entry answers to the same title, and must not get in the way.
        let gta = files(&[
            "BattlEye/BEService_x64.exe",
            "GTA5_Enhanced.exe",
            "GTA5_Enhanced_BE.exe",
            "PlayGTAV.exe",
            "Redistributables/DirectX/DXSETUP.exe",
            "Redistributables/Rockstar-Games-Epic.exe",
            "Redistributables/Rockstar-Games-Launcher.exe",
            "Redistributables/Social-Club-Setup.exe",
            "Redistributables/VCRed/vcredist_x86.exe",
            "RedistributableUninstaller.exe",
        ]);
        assert_eq!(
            pick(&apps(), "Grand Theft Auto V Enhanced", &gta, "GTAVEnhanced").as_deref(),
            Some("Grand Theft Auto V Enhanced")
        );
    }

    #[test]
    fn the_folder_named_like_the_game_wins_over_a_copy() {
        let among_us = files(&[
            "Among Us.exe",
            "crashpad_handler.exe",
            "UnityCrashHandler64.exe",
        ]);
        assert_eq!(
            pick(&apps(), "Among Us", &among_us, "AmongUs").as_deref(),
            Some("Among Us")
        );
    }

    #[test]
    fn a_game_discord_finds_anywhere_keeps_epics_folder() {
        let rocket_league = files(&[
            "Binaries/Win64/EasyAntiCheat/EasyAntiCheat_EOS_Setup.exe",
            "Binaries/Win64/Launcher.exe",
            "Binaries/Win64/RocketLeague.exe",
            "Binaries/Win64/RocketLeague_EAC.exe",
        ]);
        assert_eq!(
            pick(&apps(), "Rocket League®", &rocket_league, "rocketleague"),
            None
        );
    }

    #[test]
    fn a_shared_file_name_is_not_enough_to_claim_another_game() {
        // Ships a gta5.exe of its own, but is not Grand Theft Auto V Legacy.
        let impostor = files(&["gta5.exe"]);
        assert_eq!(pick(&apps(), "Some Other Game", &impostor, "Other"), None);
    }

    #[test]
    fn epics_folder_stays_when_discord_already_knows_it() {
        let gta = files(&["GTA5_Enhanced.exe"]);
        assert_eq!(
            pick(
                &apps(),
                "Grand Theft Auto V Enhanced",
                &gta,
                "Grand Theft Auto V Enhanced"
            ),
            None
        );
    }

    #[test]
    fn a_folder_that_would_leave_the_install_directory_is_refused() {
        assert!(!usable(".."));
        assert!(!usable("games/elsewhere"));
        assert!(!usable("c:"));
        assert!(usable("grand theft auto v enhanced"));
    }
}
