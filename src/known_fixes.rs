//! Settings particular games need, so that nobody has to find them out alone.
//!
//! The list lives in the repository under `known-fixes/` and is built into
//! Divine, which is the copy that works offline and on first start. A newer
//! revision fetched from the repository replaces it without a release: it is
//! written to the cache and read from there on, as long as its revision number
//! is higher than the built-in one. A copy that does not parse is ignored
//! rather than trusted, so the built-in list is always the floor.
//!
//! A fix is a default, never a lock. Every setting it touches has a matching
//! override in the game's own settings, and an override always wins.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::config;
use crate::log::{debug, warn};
use crate::runtimes::Runtime;

const BUILT_IN: &str = include_str!("../known-fixes/known_fixes.json");
const LATEST: &str =
    "https://raw.githubusercontent.com/NEDOSTUPN0/Divine/main/known-fixes/known_fixes.json";
/// Where the longer write-ups next to the list can be read.
const PAGES: &str = "https://github.com/NEDOSTUPN0/Divine/blob/main/known-fixes/";

#[derive(Debug, Default, Deserialize)]
struct List {
    revision: u32,
    #[serde(default)]
    games: HashMap<String, Fix>,
}

/// What one game needs. Fields a later revision adds are ignored by an older
/// Divine rather than making it throw the whole list away.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Fix {
    pub fake_epic_launcher: bool,
    pub eac_runtime: bool,
    pub battleye_runtime: bool,
    /// Why, in a sentence or two, shown on the game's page.
    pub note: String,
    /// A longer write-up under `known-fixes/`, for what a sentence cannot hold.
    pub page: String,
    /// What the link to it is called. Without one it is just "Read More".
    pub page_title: String,
}

impl Fix {
    pub fn runtime(&self, runtime: Runtime) -> bool {
        match runtime {
            Runtime::Eac => self.eac_runtime,
            Runtime::BattlEye => self.battleye_runtime,
        }
    }

    pub fn page_url(&self) -> Option<String> {
        (!self.page.is_empty()).then(|| format!("{PAGES}{}", self.page))
    }
}

/// What to write down when a switch the fixes decide is set to `chosen`:
/// nothing when that is what the fix says anyway, so the game goes back to
/// following the list, and the choice itself when it is not.
pub fn departure(chosen: bool, fixed: bool) -> Option<bool> {
    (chosen != fixed).then_some(chosen)
}

/// The fix for a game, from whichever list is newest.
pub fn for_game(app_name: &str) -> Option<Fix> {
    current().games.remove(app_name)
}

/// Fetches the list from the repository and keeps it when it is newer than the
/// one in use. Blocking. Answers whether anything changed.
pub fn refresh() -> bool {
    let Some(text) = crate::http::ask(LATEST) else {
        return false;
    };
    let latest = match serde_json::from_str::<List>(&text) {
        Ok(list) => list,
        Err(e) => {
            warn!("the known fixes in the repository do not parse: {e:#}");
            return false;
        }
    };
    let in_use = current().revision;
    if latest.revision <= in_use {
        debug!("known fixes are up to date at revision {in_use}");
        return false;
    }
    if let Err(e) = fs::write(cached(), &text) {
        warn!("cannot keep the newer known fixes: {e:#}");
        return false;
    }
    debug!(
        "known fixes moved from revision {in_use} to {}",
        latest.revision
    );
    true
}

fn current() -> List {
    newest(BUILT_IN, fs::read_to_string(cached()).ok().as_deref())
}

fn cached() -> PathBuf {
    config::cache_dir().join("known_fixes.json")
}

/// The built-in list, unless the fetched one parses and is a later revision.
fn newest(built_in: &str, fetched: Option<&str>) -> List {
    let built_in: List = serde_json::from_str(built_in).unwrap_or_default();
    match fetched.and_then(|text| serde_json::from_str::<List>(text).ok()) {
        Some(fetched) if fetched.revision > built_in.revision => fetched,
        _ => built_in,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLDER: &str = r#"{"revision": 1, "games": {"Heather": {"fake_epic_launcher": true}}}"#;
    const NEWER: &str = r#"{"revision": 2, "games": {
        "Heather": {"fake_epic_launcher": true, "battleye_runtime": true, "from_the_future": 1}
    }}"#;

    #[test]
    fn the_list_built_into_divine_parses() {
        let list: List = serde_json::from_str(BUILT_IN).expect("known_fixes.json parses");
        assert!(list.revision > 0);
        assert!(list.games["9d2d0eb64d5c44529cece33fe2a46482"].fake_epic_launcher);
    }

    #[test]
    fn a_later_revision_from_the_repository_wins() {
        let list = newest(OLDER, Some(NEWER));
        assert_eq!(list.revision, 2);
        assert!(list.games["Heather"].battleye_runtime);
    }

    #[test]
    fn a_fetched_copy_never_takes_the_list_backwards() {
        assert_eq!(newest(NEWER, Some(OLDER)).revision, 2);
        assert_eq!(newest(NEWER, Some(NEWER)).revision, 2);
    }

    #[test]
    fn a_broken_download_leaves_the_built_in_list() {
        let list = newest(OLDER, Some("<html>rate limited</html>"));
        assert_eq!(list.revision, 1);
        assert!(list.games["Heather"].fake_epic_launcher);
    }

    #[test]
    fn a_switch_put_back_where_the_fix_had_it_follows_the_list_again() {
        assert_eq!(departure(false, true), Some(false));
        assert_eq!(departure(true, true), None);
        assert_eq!(departure(true, false), Some(true));
        assert_eq!(departure(false, false), None);
    }

    #[test]
    fn a_write_up_links_into_the_repository() {
        let fix = Fix {
            page: "gta-online.md".into(),
            ..Fix::default()
        };
        assert_eq!(
            fix.page_url().as_deref(),
            Some("https://github.com/NEDOSTUPN0/Divine/blob/main/known-fixes/gta-online.md")
        );
        assert!(Fix::default().page_url().is_none());
    }
}
