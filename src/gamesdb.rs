//! Steam app id lookups through GOG's public games database.
//!
//! It maps a store's own id to every other release of the same game, so an Epic
//! app name resolves straight to a Steam app id. The umu database can do this
//! too, but only for the handful of games that need a protonfix, which leaves
//! most of a library without a ProtonDB rating. Heroic queries the same service
//! for the same reason.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::http::{self, Memo};

const RELEASES: &str = "https://gamesdb.gog.com/platforms/epic/external_releases";

#[derive(Deserialize)]
struct Response {
    game: Entry,
}

#[derive(Deserialize)]
struct Entry {
    releases: Vec<Release>,
}

#[derive(Deserialize)]
struct Release {
    platform_id: String,
    external_id: String,
}

/// Blocking, and remembered for the session including misses.
pub fn steam_app_id(app_name: &str) -> Option<u32> {
    static MEMO: Memo<String, Option<u32>> = OnceLock::new();
    http::remembered(&MEMO, app_name.to_string(), |app_name| {
        // A game the database has never heard of answers 404.
        first_steam_id(&http::ask(&format!("{RELEASES}/{app_name}"))?)
    })
}

/// Games often list several Steam releases: the base game, then editions and
/// bundles. The first one is the base game.
fn first_steam_id(body: &str) -> Option<u32> {
    serde_json::from_str::<Response>(body)
        .ok()?
        .game
        .releases
        .into_iter()
        .find(|release| release.platform_id == "steam")?
        .external_id
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::first_steam_id;

    #[test]
    fn picks_the_base_game_out_of_every_release() {
        // Trimmed from the real answer for Borderlands 3.
        let body = r#"{"game":{"releases":[
            {"platform_id":"xboxone","external_id":"9NPGXK7T2GG3"},
            {"platform_id":"steam","external_id":"397540"},
            {"platform_id":"steam","external_id":"100010"}
        ]}}"#;
        assert_eq!(first_steam_id(body), Some(397540));
    }

    #[test]
    fn ignores_games_that_never_came_to_steam() {
        let body = r#"{"game":{"releases":[{"platform_id":"epic","external_id":"Catnip"}]}}"#;
        assert_eq!(first_steam_id(body), None);
        assert_eq!(first_steam_id("not json"), None);
    }
}
