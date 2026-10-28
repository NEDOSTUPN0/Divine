//! Cached data that nothing points at any more.
//!
//! Deliberately not `legendary cleanup`. That command ends with
//! `clean_tmp_data`, which empties legendary's `tmp/` wholesale — and `tmp/`
//! is where the resume files live, so running it would quietly turn every
//! paused download into one that starts over. Its other two rules are simple
//! enough to apply here without that one.
//!
//! Every rule is written to fail towards keeping things. A cache file whose
//! owner cannot be worked out stays; a listing that comes back empty is read
//! as "could not look" rather than "nothing is owned", because the difference
//! between those two is the whole cache.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config;
use crate::models::Game;

/// What can go, and what it weighs.
#[derive(Debug, Default)]
pub struct Sweep {
    pub files: Vec<PathBuf>,
    pub bytes: u64,
}

impl Sweep {
    fn take(&mut self, path: PathBuf) {
        if let Ok(data) = fs::metadata(&path) {
            self.bytes += data.len();
            self.files.push(path);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Everything cached that no game in the library still needs.
///
/// Nothing is deleted here — the caller gets to show the number first. Games
/// named in `busy` are left alone whatever the rules say: a download reads its
/// manifest as it goes.
pub fn unused(games: &[Game], busy: &HashSet<String>) -> Sweep {
    let mut sweep = Sweep::default();
    sweep_covers(&config::covers_dir(), games, &mut sweep);
    let pages = sweep_store(&config::store_dir(), games, &mut sweep);
    sweep_screenshots(&crate::covers::screenshots_dir(), &pages, &mut sweep);
    sweep_manifest_sizes(&config::manifests_dir(), games, busy, &mut sweep);
    let legendary = config::legendary_config_dir();
    sweep_metadata(&legendary, busy, &mut sweep);
    sweep_manifests(&legendary, busy, &mut sweep);
    sweep
}

/// Deletes what the sweep found, returning what that actually freed.
pub fn remove(sweep: &Sweep) -> u64 {
    let mut freed = 0;
    for file in &sweep.files {
        let Ok(data) = fs::metadata(file) else {
            continue;
        };
        if fs::remove_file(file).is_ok() {
            freed += data.len();
        }
    }
    freed
}

/// Art for a game no longer owned, or a cover Epic has since replaced. A
/// `.part` file is a download that never finished and always goes.
fn sweep_covers(dir: &Path, games: &[Game], sweep: &mut Sweep) {
    let keep: HashSet<&str> = games
        .iter()
        .filter_map(|game| game.cover.as_ref())
        .map(|cover| cover.md5.as_str())
        .collect();
    for file in files_in(dir) {
        let wanted = file.extension().is_some_and(|kind| kind == "img")
            && stem(&file).is_some_and(|name| keep.contains(name.as_str()));
        if !wanted {
            sweep.take(file);
        }
    }
}

/// Store text for a namespace nobody in the library belongs to. What survives
/// is handed on, since it is what says which screenshots are still wanted.
fn sweep_store(dir: &Path, games: &[Game], sweep: &mut Sweep) -> Vec<PathBuf> {
    let keep: HashSet<&str> = games
        .iter()
        .map(|game| game.namespace.as_str())
        .filter(|namespace| !namespace.is_empty())
        .collect();
    let mut kept = Vec::new();
    for file in files_in(dir) {
        match stem(&file) {
            Some(name) if keep.contains(name.as_str()) => kept.push(file),
            _ => sweep.take(file),
        }
    }
    kept
}

/// Screenshots are filed under their URL, so what is still wanted is whatever
/// the surviving store pages point at. A page whose screenshots Epic has
/// swapped out leaves the old ones behind, and this is what collects them.
fn sweep_screenshots(dir: &Path, pages: &[PathBuf], sweep: &mut Sweep) {
    let mut keep: HashSet<String> = HashSet::new();
    for page in pages {
        let Ok(text) = fs::read_to_string(page) else {
            continue;
        };
        let Ok(about) = serde_json::from_str::<crate::epic::store::About>(&text) else {
            // Unreadable says nothing about the screenshots, so none of them
            // are called unused on its account.
            return;
        };
        for url in &about.screenshots {
            keep.insert(crate::covers::screenshot_name(url));
        }
    }
    for file in files_in(dir) {
        let wanted = file.extension().is_some_and(|kind| kind == "img")
            && stem(&file).is_some_and(|name| keep.contains(&name));
        if !wanted {
            sweep.take(file);
        }
    }
}

/// Divine's note of what a build costs, filed under the build it describes.
/// Epic mints a new build for every change, so these pile up one per update.
fn sweep_manifest_sizes(dir: &Path, games: &[Game], busy: &HashSet<String>, sweep: &mut Sweep) {
    let mut keep: HashSet<PathBuf> = HashSet::new();
    for game in games {
        let installed = game.installed.as_ref().map(|entry| entry.version.as_str());
        for version in [Some(game.available_version.as_str()), installed]
            .into_iter()
            .flatten()
            .filter(|version| !version.is_empty())
        {
            if let Some(path) = crate::legendary::manifest_cache_file(&game.app_name, version) {
                keep.insert(path);
            }
        }
    }
    for file in files_in(dir) {
        // The name starts with the app name, which is how a busy game's file
        // is spotted without parsing the rest of it.
        let name = stem(&file).unwrap_or_default();
        if busy.iter().any(|app_name| name.starts_with(app_name)) {
            continue;
        }
        if !keep.contains(&file) {
            sweep.take(file);
        }
    }
}

/// legendary's cached catalogue entries, one file per owned app.
fn sweep_metadata(root: &Path, busy: &HashSet<String>, sweep: &mut Sweep) {
    let owned = owned_app_names(root);
    // An empty list means the file could not be read, not that nothing is
    // owned. Wiping the lot on that reading would be quite the bug.
    if owned.is_empty() {
        return;
    }
    for file in files_in(&root.join("metadata")) {
        let Some(app_name) = stem(&file) else {
            continue;
        };
        if !owned.contains(&app_name) && !busy.contains(&app_name) {
            sweep.take(file);
        }
    }
}

/// legendary's downloaded manifests. Only the installed build's is of any use;
/// the rest are what previous updates left behind, and they are megabytes each.
fn sweep_manifests(root: &Path, busy: &HashSet<String>, sweep: &mut Sweep) {
    let installed = installed_builds(root);
    if installed.is_empty() {
        return;
    }
    for file in files_in(&root.join("manifests")) {
        let Some(name) = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if busy.iter().any(|app_name| name.starts_with(app_name)) {
            continue;
        }
        if !installed.contains(&name) {
            sweep.take(file);
        }
    }
}

/// Every app legendary knows the account owns, across platforms.
fn owned_app_names(root: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(text) = fs::read_to_string(root.join("assets.json")) else {
        return names;
    };
    let Ok(assets) = serde_json::from_str::<serde_json::Value>(&text) else {
        return names;
    };
    for entries in assets
        .as_object()
        .into_iter()
        .flat_map(|table| table.values())
    {
        for entry in entries.as_array().into_iter().flatten() {
            if let Some(name) = entry.get("app_name").and_then(serde_json::Value::as_str) {
                names.insert(name.to_string());
            }
        }
    }
    names
}

/// Manifest file names for the builds that are actually installed, spelled the
/// way legendary spells them. Both forms are minted: it writes the older one
/// for Windows and the platform-tagged one otherwise, and which is on disk
/// depends on when the game was installed.
fn installed_builds(root: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(text) = fs::read_to_string(root.join("installed.json")) else {
        return names;
    };
    let Ok(games) = serde_json::from_str::<serde_json::Value>(&text) else {
        return names;
    };
    for game in games
        .as_object()
        .into_iter()
        .flat_map(|table| table.values())
    {
        let field = |key| game.get(key).and_then(serde_json::Value::as_str);
        let (Some(app_name), Some(version)) = (field("app_name"), field("version")) else {
            continue;
        };
        let platform = field("platform").unwrap_or("Windows");
        names.insert(legendary_name(&format!("{app_name}_{version}")));
        names.insert(legendary_name(&format!("{app_name}_{platform}_{version}")));
    }
    names
}

/// legendary's own `clean_filename`, which is all it does to a manifest name.
fn legendary_name(name: &str) -> String {
    let dropped = |c: char| "<>:\"/\\|?*".contains(c);
    format!("{}.manifest", name.replace(dropped, ""))
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .collect()
}

fn stem(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_names_match_the_ones_legendary_writes() {
        // Straight off disk, from a real legendary config.
        assert_eq!(
            legendary_name("963137e4c29d4c79a81323b8fab03a40_Windows_7044"),
            "963137e4c29d4c79a81323b8fab03a40_Windows_7044.manifest"
        );
        // The characters legendary strips, stripped the same way.
        assert_eq!(legendary_name("Fowl_v1:2/3"), "Fowl_v123.manifest");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("divine-sweep-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plant(dir: &Path, name: &str, bytes: usize) -> PathBuf {
        let file = dir.join(name);
        fs::write(&file, vec![b'x'; bytes]).unwrap();
        file
    }

    fn owning(md5: &str) -> Game {
        Game {
            app_name: "Fowl".into(),
            title: "The Escapists 2".into(),
            developer: String::new(),
            description: String::new(),
            namespace: "peony".into(),
            catalog_id: String::new(),
            release_date: String::new(),
            available_version: String::new(),
            cover: Some(crate::models::Cover {
                url: String::new(),
                md5: md5.into(),
            }),
            installed: None,
            cloud_saves: false,
            dlcs: Vec::new(),
            folder: String::new(),
            partner: String::new(),
        }
    }

    /// The rule that decides deletions: art still pointed at stays, art for a
    /// cover Epic has replaced goes, and a half-finished download always goes.
    #[test]
    fn covers_are_kept_only_while_something_points_at_them() {
        let dir = scratch("covers");
        let live = plant(&dir, "abc123.img", 100);
        let stale = plant(&dir, "old999.img", 200);
        let partial = plant(&dir, "abc123.part", 50);

        let mut sweep = Sweep::default();
        sweep_covers(&dir, &[owning("abc123")], &mut sweep);

        assert!(!sweep.files.contains(&live), "the live cover was swept");
        assert!(sweep.files.contains(&stale));
        assert!(sweep.files.contains(&partial));
        assert_eq!(sweep.bytes, 250);

        assert_eq!(remove(&sweep), 250);
        assert!(live.exists());
        assert!(!stale.exists());
        assert!(!partial.exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    /// Store pages are kept per namespace, and the ones that survive are handed
    /// on because they are what says which screenshots are still wanted.
    #[test]
    fn a_store_page_survives_only_for_a_game_still_owned() {
        let dir = scratch("store");
        let mine = plant(&dir, "peony.json", 10);
        let theirs = plant(&dir, "calluna.json", 20);

        let mut sweep = Sweep::default();
        let kept = sweep_store(&dir, &[owning("abc123")], &mut sweep);

        assert_eq!(kept, vec![mine]);
        assert_eq!(sweep.files, vec![theirs]);

        fs::remove_dir_all(&dir).unwrap();
    }

    /// A page that cannot be read says nothing about screenshots, so none of
    /// them may be called unused on its account.
    #[test]
    fn an_unreadable_store_page_protects_every_screenshot() {
        let pages = scratch("pages");
        let shots = scratch("shots");
        let broken = plant(&pages, "peony.json", 0);
        fs::write(&broken, "not json at all").unwrap();
        let shot = plant(&shots, "0123456789abcdef.img", 30);

        let mut sweep = Sweep::default();
        sweep_screenshots(&shots, &[broken], &mut sweep);

        assert!(sweep.is_empty());
        assert!(shot.exists());

        fs::remove_dir_all(&pages).unwrap();
        fs::remove_dir_all(&shots).unwrap();
    }

    /// A game mid-download reads its manifest as it goes, so it is left alone
    /// whatever the rules would otherwise say.
    #[test]
    fn a_busy_game_keeps_its_files() {
        let dir = scratch("manifests");
        let busy = plant(&dir, "Fowl_Windows_7044.json", 40);

        let mut sweep = Sweep::default();
        let mut downloading = HashSet::new();
        downloading.insert("Fowl".to_string());
        sweep_manifest_sizes(&dir, &[], &downloading, &mut sweep);
        assert!(
            sweep.is_empty(),
            "a downloading game had its manifest swept"
        );

        // With nothing downloading, the same file is fair game.
        let mut sweep = Sweep::default();
        sweep_manifest_sizes(&dir, &[], &HashSet::new(), &mut sweep);
        assert_eq!(sweep.files, vec![busy]);

        fs::remove_dir_all(&dir).unwrap();
    }

    /// What `remove` reports has to be what it actually freed, since that is
    /// the number shown afterwards.
    #[test]
    fn removing_counts_only_what_really_went() {
        let dir = scratch("remove");
        let there = plant(&dir, "here.img", 64);
        let gone = dir.join("never-existed.img");

        let sweep = Sweep {
            files: vec![there.clone(), gone],
            bytes: 128,
        };
        assert_eq!(remove(&sweep), 64);
        assert!(!there.exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    /// The guard that matters most: a listing that could not be read must not
    /// read as "nothing is owned", or the sweep would take every last file.
    #[test]
    fn an_unreadable_listing_leaves_everything_alone() {
        let nowhere = Path::new("/this/does/not/exist");
        assert!(owned_app_names(nowhere).is_empty());
        assert!(installed_builds(nowhere).is_empty());

        let mut sweep = Sweep::default();
        sweep_metadata(nowhere, &HashSet::new(), &mut sweep);
        sweep_manifests(nowhere, &HashSet::new(), &mut sweep);
        // Nothing was swept, because nothing could be judged.
        assert!(sweep.is_empty());
    }
}
