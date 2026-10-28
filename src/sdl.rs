//! Selective downloads: some games ship in parts, tagged in their manifest —
//! language packs, high resolution textures. Which tags belong together is not
//! in the manifest itself; legendary keeps that mapping on its API, and only
//! consults it for the two games hardcoded in its source. We read the API for
//! every game it has data for, which is a good deal more than two.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::http;

const API: &str = "https://api.legendary.gl/v1/sdl";
/// The groups change with the game, but rarely; a week old copy is fine.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// One optional part of a game, as a set of manifest tags.
#[derive(Clone, Debug)]
pub struct Group {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// What a game can be installed without, and what it cannot.
#[derive(Clone, Debug, Default)]
pub struct Selective {
    /// Tags every install needs. Empty for games that only tag their extras.
    pub required: Vec<String>,
    pub optional: Vec<Group>,
}

#[derive(Deserialize)]
struct RawGroup {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
}

/// Whether a game has components at all, from the list legendary keeps on
/// disk. No network, so it is safe to ask while building a dialog.
pub fn has_components(legendary_config: &Path, app_name: &str) -> bool {
    api_key(legendary_config, app_name).is_some()
}

/// The groups for a game, or None when Epic ships it in one piece.
pub fn selective(legendary_config: &Path, app_name: &str) -> Result<Option<Selective>> {
    let Some(key) = api_key(legendary_config, app_name) else {
        return Ok(None);
    };
    let raw = fetch(&key)?;
    let mut result = Selective::default();
    for (key, group) in raw {
        if key == "__required" {
            result.required = group.tags;
        } else {
            result.optional.push(Group {
                name: group.name,
                description: group.description,
                tags: group.tags,
            });
        }
    }
    if result.optional.is_empty() {
        return Ok(None);
    }
    Ok(Some(result))
}

/// legendary caches the list of games with selective download data as part of
/// its version check. Keys are app name prefixes, so Fortnite's key covers its
/// per-platform names too; the Mac ones are somebody else's problem.
fn api_key(legendary_config: &Path, app_name: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Version {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(default)]
        game_overrides: Overrides,
    }
    #[derive(Deserialize, Default)]
    struct Overrides {
        #[serde(default)]
        sdl_config: BTreeMap<String, serde_json::Value>,
    }

    let text = fs::read_to_string(legendary_config.join("version.json")).ok()?;
    let version: Version = serde_json::from_str(&text).ok()?;
    version
        .data
        .game_overrides
        .sdl_config
        .into_keys()
        .filter(|key| !key.ends_with("_Mac") && app_name.starts_with(key.as_str()))
        // The longest prefix is the most specific one.
        .max_by_key(String::len)
}

fn fetch(key: &str) -> Result<BTreeMap<String, RawGroup>> {
    let cached = crate::config::sdl_dir().join(format!("{key}.json"));
    if let Ok(text) = fs::read_to_string(&cached) {
        if !is_stale(&cached) {
            if let Ok(groups) = serde_json::from_str(&text) {
                return Ok(groups);
            }
        }
        // A refresh that fails leaves the copy we already have in place.
        if let Ok(groups) = download(key, &cached) {
            return Ok(groups);
        }
        return serde_json::from_str(&text).context("parsing the cached component list");
    }
    download(key, &cached)
}

fn download(key: &str, cached: &Path) -> Result<BTreeMap<String, RawGroup>> {
    let text = http::agent(Duration::from_secs(15))
        .get(format!("{API}/{key}.json"))
        .call()
        .with_context(|| format!("asking legendary.gl about {key}"))?
        .into_body()
        .read_to_string()
        .context("reading the component list")?;
    let groups = serde_json::from_str(&text).context("parsing the component list")?;
    if let Some(parent) = cached.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(cached, &text);
    Ok(groups)
}

fn is_stale(path: &Path) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age > MAX_AGE)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[test]
    fn the_longest_matching_prefix_wins() {
        let dir = std::env::temp_dir().join("divine-sdl-test");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("version.json"),
            r#"{"data":{"game_overrides":{"sdl_config":{
                "Fortnite":12,"Fortnite_Mac":1,"UE_5.4":1,"UE_5.4_Mac":1}}}}"#,
        )
        .unwrap();

        assert_eq!(
            super::api_key(&dir, "FortniteWindows").as_deref(),
            Some("Fortnite")
        );
        assert_eq!(super::api_key(&dir, "UE_5.4"), Some("UE_5.4".to_string()));
        assert_eq!(super::api_key(&dir, "Fowl"), None);
    }
}
