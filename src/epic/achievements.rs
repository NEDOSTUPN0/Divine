//! Epic's achievements, read off the same GraphQL the store text comes from.
//!
//! Two queries, joined on the achievement's internal name. What every player
//! could earn is public and needs no token; what this account has earned needs
//! the legendary session. Epic keys both on the sandbox id, which is the
//! namespace Divine already carries on every game.
//!
//! The field names here were not guessed. Epic has introspection turned off,
//! but GraphQL still answers with one error per unknown field, so a query full
//! of candidates comes back naming exactly the ones that do not exist — which
//! is how `rarity { percent }` and `tier { name }` turned up after the obvious
//! spellings failed.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

use super::User;

const CATALOG: &str = "https://launcher.store.epicgames.com/graphql";

/// One achievement, as it should appear on screen: locked and unlocked have
/// their own name, blurb and icon, and only one of each pair is ever wanted.
#[derive(Debug, Clone)]
pub struct Achievement {
    /// Epic's internal id, e.g. "ACH_BIG_TEASE". The join key, and the only
    /// thing to show for a game that never set a display name.
    pub name: String,
    pub title: String,
    pub description: String,
    pub icon: String,
    /// Epic hides the description until it is earned.
    pub hidden: bool,
    pub xp: u32,
    /// Share of players who have this one, as a percentage.
    pub rarity: f64,
    /// "bronze", "silver" or "gold".
    pub tier: String,
    pub unlocked: bool,
    /// ISO-8601, empty while locked.
    pub unlocked_at: String,
}

/// A game's achievements and where this account stands.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub achievements: Vec<Achievement>,
    pub unlocked: u32,
    pub total: u32,
    pub xp: u32,
    pub total_xp: u32,
}

/// How to line the list up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// When it was earned.
    Earned,
    /// Share of players who have it.
    Rarity,
    /// What it is called.
    Name,
}

impl Progress {
    /// A copy of the list in the asked-for order.
    ///
    /// Ordering by when something was earned puts everything unearned at the
    /// end whichever way round it goes: those have no date to order by, and
    /// burying the earned ones under them would answer a question nobody asked.
    /// The other two orders mix locked and unlocked freely, which is the point
    /// of them.
    pub fn in_order(&self, by: Order, descending: bool) -> Vec<Achievement> {
        let mut list = self.achievements.clone();
        list.sort_by(|a, b| {
            let ordering = match by {
                Order::Earned => a.unlocked_at.cmp(&b.unlocked_at),
                Order::Rarity => a
                    .rarity
                    .partial_cmp(&b.rarity)
                    .unwrap_or(std::cmp::Ordering::Equal),
                Order::Name => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            };
            match descending {
                true => ordering.reverse(),
                false => ordering,
            }
        });
        if by == Order::Earned {
            // Stable, so this only lifts the earned ones out without disturbing
            // the order just settled on.
            list.sort_by_key(|a| !a.unlocked);
        }
        list
    }

    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.unlocked as f64 / self.total as f64
        }
    }
}

/// Everything known about a game's achievements, or `None` when Epic has none
/// for it — which is most of the library.
///
/// The catalogue half works signed out, so a game's achievements still list
/// when the session has expired; only the ticks go missing.
pub fn for_game(namespace: &str, user_file: &Path) -> Result<Option<Progress>> {
    if namespace.is_empty() {
        return Ok(None);
    }
    let Some(catalogue) = definitions(namespace)? else {
        return Ok(None);
    };

    let earned = earned(namespace, user_file).unwrap_or_default();
    let mut achievements: Vec<Achievement> = catalogue
        .achievements
        .into_iter()
        .map(|entry| {
            let it = entry.achievement;
            let mine = earned.get(&it.name);
            let unlocked = mine.is_some_and(|record| record.unlocked);
            Achievement {
                title: pick(
                    unlocked,
                    it.unlocked_display_name,
                    it.locked_display_name,
                    &it.name,
                ),
                description: pick(unlocked, it.unlocked_description, it.locked_description, ""),
                icon: pick(unlocked, it.unlocked_icon_link, it.locked_icon_link, ""),
                hidden: it.hidden,
                xp: it.xp,
                rarity: it.rarity.percent,
                tier: it.tier.name,
                unlocked,
                unlocked_at: mine
                    .and_then(|record| record.unlock_date.clone())
                    .unwrap_or_default(),
                name: it.name,
            }
        })
        .collect();

    // Earned first and newest of those first, so the last thing done is the
    // first thing seen. Locked ones follow from commonest to rarest, which is
    // the order somebody picking a next target wants.
    achievements.sort_by(|a, b| {
        b.unlocked
            .cmp(&a.unlocked)
            .then_with(|| b.unlocked_at.cmp(&a.unlocked_at))
            .then_with(|| {
                b.rarity
                    .partial_cmp(&a.rarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let unlocked = achievements.iter().filter(|a| a.unlocked).count() as u32;
    let xp = achievements
        .iter()
        .filter(|a| a.unlocked)
        .map(|a| a.xp)
        .sum();
    Ok(Some(Progress {
        total: catalogue.total.unwrap_or(achievements.len() as u32),
        total_xp: catalogue.total_xp.unwrap_or_default(),
        achievements,
        unlocked,
        xp,
    }))
}

/// Locked and unlocked carry their own text; either may be blank.
fn pick(unlocked: bool, when_unlocked: String, when_locked: String, fallback: &str) -> String {
    let chosen = if unlocked { when_unlocked } else { when_locked };
    if chosen.is_empty() {
        fallback.to_string()
    } else {
        chosen
    }
}

const CATALOGUE: &str = r#"
query achievements($sandboxId: String!) {
  Achievement {
    productAchievementsRecordBySandbox(sandboxId: $sandboxId) {
      totalAchievements
      totalProductXP
      achievements {
        achievement {
          name
          hidden
          XP
          rarity { percent }
          tier { name }
          unlockedDisplayName
          unlockedDescription
          unlockedIconLink
          lockedDisplayName
          lockedDescription
          lockedIconLink
        }
      }
    }
  }
}"#;

const EARNED: &str = r#"
query player($epicAccountId: String!, $sandboxId: String!) {
  PlayerAchievement {
    playerAchievementGameRecordsBySandbox(epicAccountId: $epicAccountId, sandboxId: $sandboxId) {
      records {
        playerAchievements {
          playerAchievement { achievementName unlocked unlockDate }
        }
      }
    }
  }
}"#;

fn definitions(namespace: &str) -> Result<Option<Record>> {
    let response: CatalogueResponse = super::graphql(
        &super::store_agent(),
        CATALOG,
        CATALOGUE,
        serde_json::json!({ "sandboxId": namespace }),
        None,
        "the achievement list",
    )?;
    let record = response.data.achievement.record;
    // Epic answers with a record full of nulls for a game that has none.
    match record.achievements.is_empty() {
        true => Ok(None),
        false => Ok(Some(record)),
    }
}

fn earned(namespace: &str, user_file: &Path) -> Result<HashMap<String, Earned>> {
    let user = User::read(user_file)?;
    let response: PlayerResponse = super::graphql(
        &super::store_agent(),
        CATALOG,
        EARNED,
        serde_json::json!({
            "epicAccountId": user.account_id,
            "sandboxId": namespace,
        }),
        Some(&user),
        "what this account has earned",
    )?;
    Ok(response
        .data
        .player
        .records
        .records
        .into_iter()
        .flat_map(|record| record.achievements)
        .map(|entry| (entry.achievement.name.clone(), entry.achievement))
        .collect())
}

#[derive(Deserialize)]
struct CatalogueResponse {
    data: CatalogueData,
}

#[derive(Deserialize)]
struct CatalogueData {
    #[serde(rename = "Achievement")]
    achievement: CatalogueHolder,
}

#[derive(Deserialize)]
struct CatalogueHolder {
    #[serde(rename = "productAchievementsRecordBySandbox")]
    record: Record,
}

/// Epic answers a game with no achievements with nulls rather than empties,
/// and `serde(default)` only covers a missing key, not a present null. Every
/// field that could come back null goes through this.
fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct Record {
    #[serde(default, rename = "totalAchievements")]
    total: Option<u32>,
    #[serde(default, rename = "totalProductXP")]
    total_xp: Option<u32>,
    #[serde(default, deserialize_with = "null_as_default")]
    achievements: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    achievement: Definition,
}

#[derive(Deserialize)]
struct Definition {
    name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    hidden: bool,
    #[serde(default, rename = "XP", deserialize_with = "null_as_default")]
    xp: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    rarity: Rarity,
    #[serde(default, deserialize_with = "null_as_default")]
    tier: Tier,
    #[serde(
        default,
        rename = "unlockedDisplayName",
        deserialize_with = "null_as_default"
    )]
    unlocked_display_name: String,
    #[serde(
        default,
        rename = "unlockedDescription",
        deserialize_with = "null_as_default"
    )]
    unlocked_description: String,
    #[serde(
        default,
        rename = "unlockedIconLink",
        deserialize_with = "null_as_default"
    )]
    unlocked_icon_link: String,
    #[serde(
        default,
        rename = "lockedDisplayName",
        deserialize_with = "null_as_default"
    )]
    locked_display_name: String,
    #[serde(
        default,
        rename = "lockedDescription",
        deserialize_with = "null_as_default"
    )]
    locked_description: String,
    #[serde(
        default,
        rename = "lockedIconLink",
        deserialize_with = "null_as_default"
    )]
    locked_icon_link: String,
}

#[derive(Deserialize, Default)]
struct Rarity {
    #[serde(default)]
    percent: f64,
}

#[derive(Deserialize, Default)]
struct Tier {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct PlayerResponse {
    data: PlayerData,
}

#[derive(Deserialize)]
struct PlayerData {
    #[serde(rename = "PlayerAchievement")]
    player: PlayerHolder,
}

#[derive(Deserialize)]
struct PlayerHolder {
    #[serde(rename = "playerAchievementGameRecordsBySandbox")]
    records: PlayerRecords,
}

#[derive(Deserialize)]
struct PlayerRecords {
    #[serde(default, deserialize_with = "null_as_default")]
    records: Vec<PlayerRecord>,
}

#[derive(Deserialize)]
struct PlayerRecord {
    #[serde(
        default,
        rename = "playerAchievements",
        deserialize_with = "null_as_default"
    )]
    achievements: Vec<PlayerEntry>,
}

#[derive(Deserialize)]
struct PlayerEntry {
    #[serde(rename = "playerAchievement")]
    achievement: Earned,
}

#[derive(Deserialize, Clone)]
pub struct Earned {
    #[serde(rename = "achievementName")]
    name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    unlocked: bool,
    #[serde(default, rename = "unlockDate")]
    unlock_date: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real answer for Fall Guys.
    const CATALOGUE_JSON: &str = r#"{"data":{"Achievement":{"productAchievementsRecordBySandbox":{
      "totalAchievements":2,"totalProductXP":1000,"achievements":[
        {"achievement":{"name":"ACH_BIG_TEASE","hidden":false,"XP":15,
          "rarity":{"percent":0.6},"tier":{"name":"bronze"},
          "unlockedDisplayName":"Big Tease","unlockedDescription":"Do the thing",
          "unlockedIconLink":"https://x/unlocked.png",
          "lockedDisplayName":"Big Tease","lockedDescription":"",
          "lockedIconLink":"https://x/locked.png"}},
        {"achievement":{"name":"ACH_INFALLIBLE","hidden":true,"XP":100,
          "rarity":{"percent":0.7},"tier":{"name":"gold"},
          "unlockedDisplayName":"Infallible","unlockedDescription":"Win it all",
          "unlockedIconLink":"https://x/u2.png",
          "lockedDisplayName":"","lockedDescription":"",
          "lockedIconLink":"https://x/l2.png"}}]}}}}"#;

    #[test]
    fn a_catalogue_answer_parses() {
        let parsed: CatalogueResponse = serde_json::from_str(CATALOGUE_JSON).unwrap();
        let record = parsed.data.achievement.record;
        assert_eq!(record.total, Some(2));
        assert_eq!(record.total_xp, Some(1000));
        assert_eq!(record.achievements.len(), 2);
        let first = &record.achievements[0].achievement;
        assert_eq!(first.name, "ACH_BIG_TEASE");
        assert_eq!(first.xp, 15);
        assert_eq!(first.rarity.percent, 0.6);
        assert_eq!(first.tier.name, "bronze");
    }

    /// Verbatim from Among Us, which has none. Epic sends nulls, not empties,
    /// and serde's `default` does not cover a null that is actually present.
    #[test]
    fn a_game_with_no_achievements_comes_back_empty() {
        let json = r#"{"data":{"Achievement":{"productAchievementsRecordBySandbox":{
          "productId":null,"sandboxId":null,"totalAchievements":null,
          "totalProductXP":null,"achievements":null}}}}"#;
        let parsed: CatalogueResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.data.achievement.record.achievements.is_empty());
    }

    #[test]
    fn a_player_who_never_played_parses() {
        let json = r#"{"data":{"PlayerAchievement":{
          "playerAchievementGameRecordsBySandbox":{"records":null}}}}"#;
        let parsed: PlayerResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.data.player.records.records.is_empty());
    }

    #[test]
    fn a_player_answer_parses() {
        let json = r#"{"data":{"PlayerAchievement":{"playerAchievementGameRecordsBySandbox":{
          "records":[{"playerAchievements":[
            {"playerAchievement":{"achievementName":"ACH_BIG_TEASE","unlocked":true,
              "unlockDate":"2022-07-05T19:40:17.304Z"}},
            {"playerAchievement":{"achievementName":"ACH_INFALLIBLE","unlocked":false,
              "unlockDate":null}}]}]}}}}"#;
        let parsed: PlayerResponse = serde_json::from_str(json).unwrap();
        let all = parsed.data.player.records.records;
        assert_eq!(all[0].achievements.len(), 2);
        assert!(all[0].achievements[0].achievement.unlocked);
        assert!(!all[0].achievements[1].achievement.unlocked);
    }

    fn sample() -> Progress {
        let make = |name: &str, rarity: f64, at: &str| Achievement {
            name: name.into(),
            title: name.into(),
            description: String::new(),
            icon: String::new(),
            hidden: false,
            xp: 10,
            rarity,
            tier: "bronze".into(),
            unlocked: !at.is_empty(),
            unlocked_at: at.into(),
        };
        Progress {
            achievements: vec![
                make("beta", 50.0, "2022-01-02T00:00:00Z"),
                make("alpha", 5.0, ""),
                make("gamma", 20.0, "2023-06-01T00:00:00Z"),
            ],
            unlocked: 2,
            total: 3,
            xp: 20,
            total_xp: 30,
        }
    }

    fn titles(list: &[Achievement]) -> Vec<&str> {
        list.iter().map(|a| a.title.as_str()).collect()
    }

    #[test]
    fn ordering_by_when_it_was_earned_keeps_the_locked_ones_last() {
        let progress = sample();
        // Newest first, then the one never earned.
        assert_eq!(
            titles(&progress.in_order(Order::Earned, true)),
            ["gamma", "beta", "alpha"]
        );
        // Oldest first, and the locked one is still last rather than first.
        assert_eq!(
            titles(&progress.in_order(Order::Earned, false)),
            ["beta", "gamma", "alpha"]
        );
    }

    #[test]
    fn the_other_orders_mix_locked_and_unlocked() {
        let progress = sample();
        assert_eq!(
            titles(&progress.in_order(Order::Rarity, false)),
            ["alpha", "gamma", "beta"]
        );
        assert_eq!(
            titles(&progress.in_order(Order::Name, false)),
            ["alpha", "beta", "gamma"]
        );
        assert_eq!(
            titles(&progress.in_order(Order::Name, true)),
            ["gamma", "beta", "alpha"]
        );
    }

    /// A locked achievement whose game left the locked text blank falls back to
    /// something rather than showing an empty row.
    #[test]
    fn missing_text_falls_back() {
        assert_eq!(pick(false, "u".into(), "l".into(), "id"), "l");
        assert_eq!(pick(true, "u".into(), "l".into(), "id"), "u");
        assert_eq!(pick(false, "u".into(), String::new(), "id"), "id");
    }
}
