//! Lookups against the umu games database.
//!
//! umu keys its protonfixes by game id, and the fix a game needs is often the
//! difference between running and not. Grand Theft Auto V is the obvious
//! example: its fix removes SteamAppId from the environment, without which the
//! Rockstar launcher tries to initialise the Steam API and dies with
//! "Steam failed to initialize". Heroic resolves ids the same way.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::http::{self, Memo};
use crate::log::warn;

const API: &str = "https://umu.openwinecomponents.org/umu_api.php";

/// The store Divine always launches from, as umu's protonfixes name it.
pub const STORE: &str = "egs";
/// The id a game the database does not know runs under.
///
/// One of its own rather than umu's neutral "umu-0". umu hands everything after
/// "umu-" to Proton as `SteamGameId`, and Proton names the game's X11 window
/// `steam_app_<that>`: with one id for every such game, no desktop could tell
/// their windows apart or find the entry each belongs to. protonfixes looks a
/// fix up by this id and finds none, as it did for "umu-0". The "egs_" keeps it
/// from ever being all digits, which Proton would take for a Steam app.
pub fn own_id(app_name: &str) -> String {
    let name: String = app_name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    format!("umu-egs_{name}")
}

#[derive(Deserialize)]
struct Entry {
    umu_id: String,
}

/// umu ids are "umu-<steam app id>" for anything that also exists on Steam,
/// which is what ProtonDB needs. Ids like "umu-0" carry no number.
pub fn steam_app_id(game_id: &str) -> Option<u32> {
    game_id
        .strip_prefix("umu-")?
        .parse()
        .ok()
        .filter(|id| *id > 0)
}

/// Resolves an Epic app name to a umu game id, or `None` when the database has
/// no entry for it. Blocking.
///
/// The answer is kept on disk, because the id is more than a lookup key: it
/// picks the game's protonfixes and names its window, which the menu entry
/// has to match. Asked afresh every run, an hour without the database would
/// have the game start without its fixes and its window renamed. So an id,
/// once found, is kept for good; a miss is asked again after a week, since the
/// database grows; and a question that got no answer is never written down.
pub fn game_id(app_name: &str) -> Option<String> {
    static MEMO: Memo<String, Option<String>> = OnceLock::new();
    http::remembered(&MEMO, app_name.to_lowercase(), |codename| {
        resolve(&cache_file(), codename, now(), ask)
    })
}

/// How long the database's "no such game" is believed.
const RECHECK_MISS: u64 = 7 * 24 * 60 * 60;

/// What the database said about one game, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Known {
    id: Option<String>,
    /// Seconds since the epoch.
    asked: u64,
}

fn resolve(
    file: &Path,
    codename: &str,
    now: u64,
    ask: impl FnOnce(&str) -> Option<Option<String>>,
) -> Option<String> {
    let mut known = load(file);
    if let Some(entry) = known.get(codename) {
        if entry.id.is_some() || now.saturating_sub(entry.asked) < RECHECK_MISS {
            return entry.id.clone();
        }
    }
    let Some(answer) = ask(codename) else {
        // No answer is not "no such game": whatever was known stands.
        return known.get(codename).and_then(|entry| entry.id.clone());
    };
    known.insert(
        codename.to_string(),
        Known {
            id: answer.clone(),
            asked: now,
        },
    );
    if let Err(e) = save(file, &known) {
        warn!("cannot keep the umu ids: {e:#}");
    }
    answer
}

/// The database's answer: an id, or `Some(None)` when it has no entry. `None`
/// when there was no answer to be had, which must not be taken for a miss.
fn ask(codename: &str) -> Option<Option<String>> {
    let body = http::ask_with(API, &[("codename", codename), ("store", STORE)])?;
    // An unknown game comes back as an empty list.
    let entries: Vec<Entry> = serde_json::from_str(&body).ok()?;
    Some(entries.into_iter().next().map(|entry| entry.umu_id))
}

fn load(file: &Path) -> HashMap<String, Known> {
    fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Through a temporary file: the window and a shortcut launch can both be
/// writing, and either one reading half a file would forget every id.
fn save(file: &Path, known: &HashMap<String, Known>) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let partial = file.with_extension(format!("{}.partial", std::process::id()));
    fs::write(&partial, serde_json::to_string_pretty(known)?)
        .with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, file).with_context(|| format!("writing {}", file.display()))
}

fn cache_file() -> PathBuf {
    crate::config::cache_dir().join("umu-ids.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// What umu is doing about Proton, as far as its own output says.
///
/// A launch stops for as long as this takes — a whole build is a gigabyte and
/// a half, a patch a great deal less — and from the outside the game simply
/// takes a long time to start. There is no percentage to be had: umu says what
/// it is doing and then says when it is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetching {
    /// Pulling down a whole build, named as the file it arrives in.
    Downloading(String),
    /// Bringing the one on disk up to the newest release.
    Patching,
    Extracting(String),
    /// Nothing more to wait for: Proton is ready and the game is starting.
    Ready,
}

/// Reads one line umu wrote and says what stage of fetching Proton it means.
///
/// The wording is umu's own, from `umu_proton.py`: `log.info("Downloading %s...")`,
/// `log.info("Extracting %s...")`, `log.info("%s is up to date")`,
/// `log.info("Using %s")` and `log.info("%s is OK, applying partial update...")`.
pub fn fetching(line: &str) -> Option<Fetching> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("Downloading ") {
        return Some(Fetching::Downloading(trim_ellipsis(rest)));
    }
    if let Some(rest) = line.strip_prefix("Extracting ") {
        return Some(Fetching::Extracting(trim_ellipsis(rest)));
    }
    if line.ends_with("applying partial update...") {
        return Some(Fetching::Patching);
    }
    // Both of these mean the waiting is over: the first when there was nothing
    // to fetch, the second once whatever was fetched is in place.
    if line.ends_with("is up to date") || line.starts_with("Using ") {
        return Some(Fetching::Ready);
    }
    None
}

fn trim_ellipsis(name: &str) -> String {
    name.trim().trim_end_matches("...").to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let file =
                std::env::temp_dir().join(format!("divine-umu-{name}-{}.json", std::process::id()));
            let _ = fs::remove_file(&file);
            Scratch(file)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    const DAY: u64 = 24 * 60 * 60;

    /// The id GTA V Enhanced has in the database today.
    fn gta(_: &str) -> Option<Option<String>> {
        Some(Some("umu-3240220".into()))
    }

    fn offline(_: &str) -> Option<Option<String>> {
        None
    }

    fn unknown(_: &str) -> Option<Option<String>> {
        Some(None)
    }

    #[test]
    fn an_id_once_found_survives_the_database_being_down() {
        let cache = Scratch::new("down");
        assert_eq!(
            super::resolve(&cache.0, "gta", 0, gta).as_deref(),
            Some("umu-3240220")
        );
        assert_eq!(
            super::resolve(&cache.0, "gta", 365 * DAY, offline).as_deref(),
            Some("umu-3240220")
        );
    }

    #[test]
    fn a_found_id_is_not_asked_for_again() {
        let cache = Scratch::new("found");
        super::resolve(&cache.0, "gta", 0, gta);
        let asked = |_: &str| -> Option<Option<String>> { panic!("asked again") };
        super::resolve(&cache.0, "gta", 30 * DAY, asked);
    }

    #[test]
    fn no_answer_is_not_written_down_as_no_such_game() {
        let cache = Scratch::new("offline");
        assert_eq!(super::resolve(&cache.0, "gta", 0, offline), None);
        // Back online the same session's next run asks, and finds it.
        assert_eq!(
            super::resolve(&cache.0, "gta", 1, gta).as_deref(),
            Some("umu-3240220")
        );
    }

    #[test]
    fn a_miss_is_believed_for_a_week_then_asked_again() {
        let cache = Scratch::new("miss");
        assert_eq!(super::resolve(&cache.0, "fowl", 0, unknown), None);
        let asked = |_: &str| -> Option<Option<String>> { panic!("asked too soon") };
        assert_eq!(super::resolve(&cache.0, "fowl", 6 * DAY, asked), None);
        // The database has learned the game since.
        let learned = |_: &str| Some(Some("umu-641990".to_string()));
        assert_eq!(
            super::resolve(&cache.0, "fowl", 8 * DAY, learned).as_deref(),
            Some("umu-641990")
        );
    }
    use super::Fetching;

    /// umu's own wording, from the calls in `umu_proton.py` that surround a
    /// Proton fetch. A launch waits on all of this without saying so.
    #[test]
    fn a_proton_fetch_is_read_off_umus_own_words() {
        let step = super::fetching;
        assert_eq!(
            step("Downloading GE-Proton11-6.tar.gz..."),
            Some(Fetching::Downloading("GE-Proton11-6.tar.gz".into()))
        );
        assert_eq!(
            step("Extracting GE-Proton11-6.tar.gz..."),
            Some(Fetching::Extracting("GE-Proton11-6.tar.gz".into()))
        );
        assert_eq!(
            step("GE-Latest is OK, applying partial update..."),
            Some(Fetching::Patching)
        );
        assert_eq!(step("GE-Latest is up to date"), Some(Fetching::Ready));
        assert_eq!(step("Using GE-Latest"), Some(Fetching::Ready));
        // Everything else on that stream is about the launch, not the fetch.
        assert_eq!(step("Running 'steamrt4' using runtime 'host'"), None);
        assert_eq!(step("umu-launcher version 1.4.4"), None);
    }

    use super::steam_app_id;

    #[test]
    fn reads_the_steam_id_out_of_a_umu_id() {
        assert_eq!(steam_app_id("umu-271590"), Some(271590));
        // Divine's own ids and anything non-numeric map to no Steam game.
        assert_eq!(steam_app_id(&super::own_id("Fowl")), None);
        assert_eq!(steam_app_id("umu-0"), None);
        assert_eq!(steam_app_id("umu-default"), None);
        assert_eq!(steam_app_id("271590"), None);
    }
}
