//! Shapes of legendary's JSON output and the merged library entry the UI uses.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct KeyImage {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub md5: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Category {
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReleaseInfo {
    #[serde(default, rename = "dateAdded")]
    pub date_added: String,
    /// The app name of the release. On a DLC's `mainGameItem` this is the base
    /// game it belongs to, which is the only link between the two.
    #[serde(default, rename = "appId")]
    pub app_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MainGameItem {
    #[serde(default, rename = "releaseInfo")]
    pub release_info: Vec<ReleaseInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Metadata {
    #[serde(default)]
    pub developer: String,
    /// Epic's id for the product this game belongs to, and the way to its
    /// store page.
    #[serde(default)]
    pub namespace: String,
    /// Epic's id for this catalogue entry. Only collections need it, and they
    /// need it alongside the namespace rather than instead of it.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "creationDate")]
    pub creation_date: String,
    #[serde(default, rename = "releaseInfo")]
    pub release_info: Vec<ReleaseInfo>,
    #[serde(default, rename = "keyImages")]
    pub key_images: Vec<KeyImage>,
    #[serde(default)]
    pub categories: Vec<Category>,
    /// Present on DLC entries only, which is how we spot them offline.
    #[serde(default, rename = "mainGameItem")]
    pub main_game_item: Option<MainGameItem>,
    /// Epic's grab bag of extra fields. The one we read is CloudSaveFolder,
    /// whose presence is what "supports cloud saves" means.
    #[serde(default, rename = "customAttributes")]
    pub custom_attributes: HashMap<String, CustomAttribute>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CustomAttribute {
    #[serde(default)]
    pub value: String,
}

impl Metadata {
    /// The folder legendary installs the game into, relative to the base path.
    /// Its own word for it, so a half-finished download can be found on disk.
    pub fn folder_name(&self, app_name: &str) -> String {
        match self.custom_attributes.get("FolderName") {
            Some(attr) if !attr.value.is_empty() => attr.value.clone(),
            _ => app_name.to_string(),
        }
    }

    /// Epic sells some games it does not host: the entitlement is Epic's, the
    /// files are the partner's. Empty for everything Epic ships itself.
    pub fn partner(&self) -> String {
        self.custom_attributes
            .get("partnerLinkType")
            .map(|attr| attr.value.clone())
            .unwrap_or_default()
    }

    pub fn supports_cloud_saves(&self) -> bool {
        self.custom_attributes
            .get("CloudSaveFolder")
            .is_some_and(|attr| !attr.value.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AssetInfo {
    #[serde(default)]
    pub build_version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListEntry {
    pub app_name: String,
    pub app_title: String,
    #[serde(default)]
    pub metadata: Metadata,
    /// Keyed by platform; the build version here is what an install would get.
    #[serde(default)]
    pub asset_infos: HashMap<String, AssetInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstalledEntry {
    pub app_name: String,
    #[serde(default)]
    pub title: String,
    pub install_path: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub install_size: u64,
    /// Components the game was installed with. Empty means the lot of it.
    #[serde(default)]
    pub install_tags: Vec<String>,
}

/// Cover art we settled on for a game.
#[derive(Debug, Clone)]
pub struct Cover {
    pub url: String,
    pub md5: String,
}

/// One library tile: catalogue metadata plus install state, if any.
#[derive(Debug, Clone)]
pub struct Game {
    pub app_name: String,
    pub title: String,
    pub developer: String,
    pub description: String,
    /// Epic's product id, which is how the store page is found.
    pub namespace: String,
    /// Epic's catalogue id, which putting the game in a collection needs.
    pub catalog_id: String,
    /// Just the date part, or empty when Epic gave us nothing usable.
    pub release_date: String,
    /// Build version Epic currently offers, for comparing against the install.
    pub available_version: String,
    pub cover: Option<Cover>,
    pub installed: Option<InstalledEntry>,
    /// Epic keeps saves for this game, so it is worth syncing them.
    pub cloud_saves: bool,
    /// Owned add-ons, which Epic ships as apps of their own.
    pub dlcs: Vec<Dlc>,
    /// Where legendary puts it under the install folder. Epic's name for the
    /// directory, which is how a cancelled download is found again.
    pub folder: String,
    /// Epic's word for whoever actually hosts this game, when it is not Epic.
    /// "ubisoft" is the one Divine can do something about; anything else is
    /// only worth saying out loud so the page does not promise an install that
    /// cannot happen.
    pub partner: String,
}

/// One owned add-on. It installs and uninstalls like a game, but only next to
/// the base game it belongs to.
#[derive(Debug, Clone)]
pub struct Dlc {
    pub app_name: String,
    pub title: String,
    pub installed: bool,
}

impl Metadata {
    /// Epic ships several crops per game; the tall one is what a grid wants.
    pub fn cover(&self) -> Option<Cover> {
        const PREFERRED: [&str; 4] = [
            "DieselGameBoxTall",
            "OfferImageTall",
            "DieselGameBox",
            "OfferImageWide",
        ];
        let pick = PREFERRED
            .iter()
            .find_map(|k| self.key_images.iter().find(|i| &i.kind == k))
            .or_else(|| self.key_images.first())?;

        // md5 is missing on a few entries; the URL is a stable enough cache key.
        let md5 = if pick.md5.is_empty() {
            format!("{:x}", fnv1a(&pick.url))
        } else {
            pick.md5.clone()
        };
        Some(Cover {
            url: pick.url.clone(),
            md5,
        })
    }

    /// Epic dates look like "2024-06-17T21:27:47.128Z"; the day is enough.
    pub fn release_date(&self) -> String {
        self.release_info
            .first()
            .map(|info| info.date_added.as_str())
            .filter(|date| !date.is_empty())
            .unwrap_or(&self.creation_date)
            .split('T')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    pub fn is_game(&self) -> bool {
        self.main_game_item.is_none() && self.categories.iter().any(|c| c.path == "games")
    }

    /// For a DLC entry, the app name of the game it belongs to.
    pub fn base_app_name(&self) -> Option<&str> {
        let release = self.main_game_item.as_ref()?.release_info.first()?;
        (!release.app_id.is_empty()).then_some(release.app_id.as_str())
    }
}

impl Game {
    pub fn is_installed(&self) -> bool {
        self.installed.is_some()
    }

    /// Installed, but its folder is not there: a drive that is not mounted, or a
    /// folder removed by hand. legendary never checks, so the game stays
    /// installed as far as it knows, and an update run now would write into an
    /// empty mount point on whatever disk lies underneath.
    pub fn folder_missing(&self) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|installed| !Path::new(&installed.install_path).is_dir())
    }

    /// Epic hands over an entitlement rather than files, so there is nothing
    /// here to download.
    pub fn is_partner_hosted(&self) -> bool {
        !self.partner.is_empty()
    }

    /// The one partner Divine can claim a game on.
    pub fn is_ubisoft(&self) -> bool {
        self.partner.eq_ignore_ascii_case("ubisoft")
    }

    /// Whether the game answers to what was typed in the search bar.
    ///
    /// The developer counts as well as the title: Epic's names rarely say who
    /// made the game, so a studio is otherwise unsearchable.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.is_empty()
            || self.title.to_lowercase().contains(&needle)
            || self.developer.to_lowercase().contains(&needle)
    }

    /// Epic offers a build the installed one does not match.
    pub fn update_available(&self) -> bool {
        match &self.installed {
            Some(installed) => {
                !self.available_version.is_empty() && installed.version != self.available_version
            }
            None => false,
        }
    }
}

/// Joins the catalogue with the install list, sorted by title.
pub fn merge(list: Vec<ListEntry>, installed: Vec<InstalledEntry>) -> Vec<Game> {
    // Epic ships add-ons as apps of their own, mixed into the same list. They
    // are pulled out first and handed to the game they belong to.
    let (dlc_entries, list): (Vec<ListEntry>, Vec<ListEntry>) = list
        .into_iter()
        .partition(|e| e.metadata.base_app_name().is_some());
    let mut dlcs: HashMap<String, Vec<Dlc>> = HashMap::new();
    for entry in dlc_entries {
        let Some(base) = entry.metadata.base_app_name() else {
            continue;
        };
        dlcs.entry(base.to_string()).or_default().push(Dlc {
            installed: installed.iter().any(|i| i.app_name == entry.app_name),
            app_name: entry.app_name.clone(),
            title: entry.app_title.clone(),
        });
    }
    for list in dlcs.values_mut() {
        list.sort_by_key(|dlc| dlc.title.to_lowercase());
    }

    let mut games: Vec<Game> = list
        .into_iter()
        .map(|e| Game {
            cover: e.metadata.cover(),
            developer: e.metadata.developer.clone(),
            description: e.metadata.description.trim().to_string(),
            namespace: e.metadata.namespace.clone(),
            catalog_id: e.metadata.id.clone(),
            release_date: e.metadata.release_date(),
            available_version: e
                .asset_infos
                .get("Windows")
                .map(|asset| asset.build_version.clone())
                .unwrap_or_default(),
            installed: installed.iter().find(|i| i.app_name == e.app_name).cloned(),
            cloud_saves: e.metadata.supports_cloud_saves(),
            dlcs: dlcs.remove(&e.app_name).unwrap_or_default(),
            partner: e.metadata.partner(),
            folder: e.metadata.folder_name(&e.app_name),
            app_name: e.app_name,
            title: e.app_title,
        })
        .collect();
    // Installed games come first: they are the ones there to be played.
    games.sort_by_key(|g| (!g.is_installed(), g.title.to_lowercase()));
    games
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The folder name decides what a cancelled download deletes, so falling
    /// back to the app name matters more than it looks: an empty one would
    /// resolve to the install directory itself.
    #[test]
    fn a_missing_folder_name_falls_back_to_the_app_name() {
        let named: Metadata =
            serde_json::from_str(r#"{"customAttributes": {"FolderName": {"value": "Beholder1"}}}"#)
                .unwrap();
        assert_eq!(named.folder_name("6fa12837"), "Beholder1");

        let blank: Metadata =
            serde_json::from_str(r#"{"customAttributes": {"FolderName": {"value": ""}}}"#).unwrap();
        assert_eq!(blank.folder_name("6fa12837"), "6fa12837");

        let absent: Metadata = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.folder_name("6fa12837"), "6fa12837");
    }

    /// Trimmed from a real `legendary list --json`: the base game, and the DLC
    /// entry that points back at it through its release info.
    const LIST: &str = r#"[
        {"app_name":"8935bb3e","app_title":"Alien: Isolation","metadata":{"categories":[{"path":"games"}]}},
        {"app_name":"6b61b369","app_title":"Alien: Isolation - Last Survivor","metadata":{
            "mainGameItem":{"releaseInfo":[{"appId":"8935bb3e"}]}
        }}
    ]"#;

    #[test]
    fn add_ons_land_on_the_game_they_belong_to() {
        let list: Vec<ListEntry> = serde_json::from_str(LIST).expect("valid list");
        let installed = vec![InstalledEntry {
            app_name: "6b61b369".into(),
            title: "Alien: Isolation - Last Survivor".into(),
            install_path: "/games/alien".into(),
            version: "1".into(),
            install_size: 1,
            install_tags: Vec::new(),
        }];

        let games = merge(list, installed);

        // The add-on is not a library tile of its own.
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "Alien: Isolation");
        assert_eq!(games[0].dlcs.len(), 1);
        assert!(games[0].dlcs[0].installed);
        // The base game itself is not installed, only its add-on is.
        assert!(!games[0].is_installed());
    }

    #[test]
    fn a_game_on_a_drive_that_is_gone_says_so() {
        let list: Vec<ListEntry> = serde_json::from_str(LIST).expect("valid list");
        let here = std::env::temp_dir();
        let installed = |path: &Path| InstalledEntry {
            app_name: "8935bb3e".into(),
            title: "Alien: Isolation".into(),
            install_path: path.display().to_string(),
            version: "1".into(),
            install_size: 1,
            install_tags: Vec::new(),
        };

        let present = merge(list.clone(), vec![installed(&here)]);
        assert!(!present[0].folder_missing());

        let gone = merge(
            list.clone(),
            vec![installed(&here.join("divine-unmounted-drive"))],
        );
        assert!(gone[0].folder_missing());

        let never_installed = merge(list, Vec::new());
        assert!(!never_installed[0].folder_missing());
    }

    /// Searching by studio is the whole point of matching the developer: not
    /// one of these titles contains the word typed.
    #[test]
    fn the_search_finds_a_game_by_its_developer() {
        let list: Vec<ListEntry> = serde_json::from_str(
            r#"[{"app_name":"8935bb3e","app_title":"Alien: Isolation",
                 "metadata":{"developer":"Creative Assembly","categories":[{"path":"games"}]}}]"#,
        )
        .expect("valid list");
        let game = &merge(list, Vec::new())[0];

        assert!(game.matches("creative"));
        assert!(game.matches("Assembly"));
        assert!(game.matches("isolation"));
        // Whitespace around it is what a paste leaves behind, and an empty
        // search is not a filter at all.
        assert!(game.matches("  alien "));
        assert!(game.matches(""));
        assert!(!game.matches("ubisoft"));
    }

    /// What the size on a card actually reads as. The unit steps at 1024, and
    /// the last one has to stop rather than run off the end of the list.
    #[test]
    fn a_size_reads_the_way_a_store_page_writes_it() {
        assert_eq!(human_size(0), "0.0 B");
        assert_eq!(human_size(512), "512.0 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(70 * 1024 * 1024 * 1024), "70.0 GiB");
        // The unit list ends at TiB, so anything larger keeps counting in TiB
        // rather than running past the end of it.
        assert_eq!(human_size(u64::MAX), "16777216.0 TiB");
    }
}
