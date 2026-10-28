//! Lookups against the umu games database.
//!
//! umu keys its protonfixes by game id, and the fix a game needs is often the
//! difference between running and not. Grand Theft Auto V is the obvious
//! example: its fix removes SteamAppId from the environment, without which the
//! Rockstar launcher tries to initialise the Steam API and dies with
//! "Steam failed to initialize". Heroic resolves ids the same way.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::http::{self, Memo};

const API: &str = "https://umu.openwinecomponents.org/umu_api.php";

/// The store Divine always launches from, as umu's protonfixes name it.
pub const STORE: &str = "egs";
/// umu's neutral id: valid, but matches no game-specific fix.
pub const NEUTRAL_ID: &str = "umu-0";

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
/// no entry for it. Blocking, and remembered for the session including misses.
pub fn game_id(app_name: &str) -> Option<String> {
    static MEMO: Memo<String, Option<String>> = OnceLock::new();
    http::remembered(&MEMO, app_name.to_lowercase(), |codename| {
        let body = http::ask_with(API, &[("codename", codename), ("store", STORE)])?;
        serde_json::from_str::<Vec<Entry>>(&body)
            .ok()?
            .into_iter()
            .next()
            .map(|entry| entry.umu_id)
    })
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
        // The neutral id and anything non-numeric map to no Steam game.
        assert_eq!(steam_app_id(super::NEUTRAL_ID), None);
        assert_eq!(steam_app_id("umu-default"), None);
        assert_eq!(steam_app_id("271590"), None);
    }
}
