//! ProtonDB compatibility ratings.
//!
//! ProtonDB is keyed by Steam app id, which Epic games obviously do not have.
//! The umu database already maps an Epic codename to an id like "umu-271590",
//! and that number is the Steam app id, so a game umu knows about can be looked
//! up here too. Games it does not know simply have no rating to show.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::http::{self, Memo};

const SUMMARIES: &str = "https://www.protondb.com/api/v1/reports/summaries";

#[derive(Debug, Clone, Deserialize)]
pub struct Summary {
    pub tier: String,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub total: u32,
}

impl Summary {
    /// "gold" reads better as "Gold" on a badge.
    pub fn label(&self) -> String {
        let mut chars = self.tier.chars();
        match chars.next() {
            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
            None => "Unknown".to_string(),
        }
    }

    pub fn css_class(&self) -> String {
        format!("protondb-{}", self.tier.to_lowercase())
    }

    pub fn tooltip(&self) -> String {
        format!(
            "ProtonDB rating from {} reports, {} confidence",
            self.total, self.confidence
        )
    }
}

/// Blocking, and remembered for the session including misses.
pub fn summary(steam_app_id: u32) -> Option<Summary> {
    static MEMO: Memo<u32, Option<Summary>> = OnceLock::new();
    http::remembered(&MEMO, steam_app_id, |id| {
        // A game with no reports answers 404, which is not worth logging.
        serde_json::from_str(&http::ask(&format!("{SUMMARIES}/{id}.json"))?).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ProtonDB writes its tiers in lower case; a badge reads better capitalised
    /// and needs a css class per tier to be coloured.
    #[test]
    fn a_tier_becomes_a_badge() {
        let gold = Summary {
            tier: "gold".into(),
            confidence: "strong".into(),
            total: 2272,
        };
        assert_eq!(gold.label(), "Gold");
        assert_eq!(gold.css_class(), "protondb-gold");
        assert_eq!(
            gold.tooltip(),
            "ProtonDB rating from 2272 reports, strong confidence"
        );
    }

    /// A summary with no tier at all still has to render something.
    #[test]
    fn a_missing_tier_says_so_rather_than_showing_nothing() {
        let empty = Summary {
            tier: String::new(),
            confidence: String::new(),
            total: 0,
        };
        assert_eq!(empty.label(), "Unknown");
    }
}
