//! Anti-cheat runtimes for games shipping EasyAntiCheat or BattlEye.
//!
//! Proton needs the Linux build of each anti-cheat next to the game, and picks
//! it up from PROTON_EAC_RUNTIME / PROTON_BATTLEYE_RUNTIME. Lutris builds and
//! hosts both and publishes an index of them; Heroic pulls the same two from
//! the same place, so Divine does too rather than repackage them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::http;

const INDEX_URL: &str = "https://lutris.net/api/runtimes";
/// Written next to the unpacked files, holding the index's timestamp for the
/// build on disk. That is how a stale runtime is noticed later.
const VERSION_FILE: &str = "current_version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Eac,
    BattlEye,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    url: String,
    #[serde(default)]
    created_at: String,
}

impl Runtime {
    pub const ALL: [Runtime; 2] = [Runtime::Eac, Runtime::BattlEye];

    /// The name Lutris publishes it under, which is also the directory name.
    pub fn name(self) -> &'static str {
        match self {
            Runtime::Eac => "eac_runtime",
            Runtime::BattlEye => "battleye_runtime",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Runtime::Eac => "EasyAntiCheat Runtime",
            Runtime::BattlEye => "BattlEye Runtime",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Runtime::Eac => "PROTON_EAC_RUNTIME",
            Runtime::BattlEye => "PROTON_BATTLEYE_RUNTIME",
        }
    }

    pub fn dir(self) -> PathBuf {
        crate::config::runtimes_dir().join(self.name())
    }

    pub fn is_installed(self) -> bool {
        self.dir().join(VERSION_FILE).is_file()
    }

    fn installed_version(self) -> String {
        fs::read_to_string(self.dir().join(VERSION_FILE)).unwrap_or_default()
    }
}

/// Downloads the runtime unless it is already there. Touches the network only
/// when something is missing, so launching a game stays offline-friendly.
pub fn ensure(runtime: Runtime, on_progress: &dyn Fn(u64, u64)) -> Result<()> {
    if runtime.is_installed() {
        return Ok(());
    }
    install(runtime, &fetch_entry(runtime)?, on_progress)
}

/// Same, but also replaces a build older than the one Lutris publishes now.
/// Blocking, and `on_progress` gets (downloaded, total) bytes while it runs.
pub fn update(runtime: Runtime, on_progress: &dyn Fn(u64, u64)) -> Result<()> {
    let entry = fetch_entry(runtime)?;
    if runtime.is_installed() && runtime.installed_version() == entry.created_at {
        return Ok(());
    }
    install(runtime, &entry, on_progress)
}

fn fetch_entry(runtime: Runtime) -> Result<Entry> {
    let body = http::agent(Duration::from_secs(30))
        .get(INDEX_URL)
        .call()
        .context("querying the Lutris runtime index")?
        .into_body()
        .read_to_string()
        .context("reading the Lutris runtime index")?;

    serde_json::from_str::<Vec<Entry>>(&body)
        .context("parsing the Lutris runtime index")?
        .into_iter()
        .find(|entry| entry.name == runtime.name())
        .ok_or_else(|| anyhow!("{} is not in the Lutris runtime index", runtime.name()))
}

fn install(runtime: Runtime, entry: &Entry, on_progress: &dyn Fn(u64, u64)) -> Result<()> {
    let dir = runtime.dir();
    let parent = crate::config::runtimes_dir();
    fs::create_dir_all(&parent).with_context(|| format!("creating {}", parent.display()))?;

    let archive = parent.join(format!(".{}.part", runtime.name()));
    crate::proton::download(&entry.url, &archive, 0, on_progress)
        .inspect_err(|_| drop(fs::remove_file(&archive)))?;

    // A half replaced runtime is worse than none, so the old one goes first.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    // The tarballs wrap everything in one directory that Proton does not expect
    // to see, which is why it is stripped. Heroic unpacks them the same way.
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&dir)
        .arg("--strip-components=1")
        .status()
        .context("running tar")?;
    let _ = fs::remove_file(&archive);
    if !status.success() {
        bail!("tar failed to unpack {}", runtime.name());
    }

    fs::write(dir.join(VERSION_FILE), &entry.created_at)
        .with_context(|| format!("marking {} as installed", runtime.name()))
}
