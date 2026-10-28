//! Downloading Proton builds from GitHub releases and unpacking them into the
//! compatibility tool directory Divine manages.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::http;

/// How far back to look for a build. Ten is several months of releases and
/// well short of the page GitHub would rather not hand out unauthenticated.
const RELEASES_PER_SOURCE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    GeProton,
    CachyOs,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::GeProton, Source::CachyOs];

    pub fn label(self) -> &'static str {
        match self {
            Source::GeProton => "GE-Proton",
            Source::CachyOs => "Proton CachyOS",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Source::GeProton => "Community build with extra codecs and game fixes",
            Source::CachyOs => "Performance-focused build, v3 needs an AVX2 capable CPU",
        }
    }

    fn repo(self) -> &'static str {
        match self {
            Source::GeProton => "GloriousEggroll/proton-ge-custom",
            Source::CachyOs => "CachyOS/proton-cachyos",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Release {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct GhRelease {
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

/// Keeps only the tarballs built for the architecture we are running on.
/// Assets without any arch marker predate multi-arch releases and are x86_64.
fn matches_arch(name: &str) -> bool {
    built_for(name, std::env::consts::ARCH)
}

fn built_for(name: &str, arch: &str) -> bool {
    if !(name.ends_with(".tar.gz") || name.ends_with(".tar.xz")) {
        return false;
    }
    let is_arm = name.contains("aarch64") || name.contains("arm64");
    let is_x86 = name.contains("x86_64");
    match arch {
        "x86_64" => is_x86 || !is_arm,
        "aarch64" => is_arm,
        _ => false,
    }
}

fn strip_extension(name: &str) -> &str {
    name.strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".tar.xz"))
        .unwrap_or(name)
}

/// The published builds, newest first. GitHub's unauthenticated API is fine
/// here: this only ever reads a release list, which needs no token.
pub fn fetch_releases(source: Source) -> Result<Vec<Release>> {
    let url = format!(
        "https://api.github.com/repos/{}/releases?per_page={RELEASES_PER_SOURCE}",
        source.repo()
    );
    let body = http::agent(Duration::from_secs(30))
        .get(&url)
        .call()
        .with_context(|| format!("querying {}", source.repo()))?
        .into_body()
        .read_to_string()
        .context("reading the release list")?;

    let releases: Vec<GhRelease> =
        serde_json::from_str(&body).context("parsing the GitHub release list")?;

    Ok(releases
        .into_iter()
        .flat_map(|release| release.assets)
        .filter(|asset| matches_arch(&asset.name))
        .map(|asset| Release {
            name: strip_extension(&asset.name).to_string(),
            url: asset.browser_download_url,
            size: asset.size,
        })
        .collect())
}

/// Downloads and unpacks a build. `on_progress` gets (downloaded, total) bytes;
/// total is 0 while the size is unknown.
pub fn install(release: &Release, dir: &Path, on_progress: &dyn Fn(u64, u64)) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let archive = dir.join(format!(".{}.part", release.name));
    download(&release.url, &archive, release.size, on_progress)
        .inspect_err(|_| drop(fs::remove_file(&archive)))?;

    // tar autodetects gzip and xz, and is everywhere; pulling in decompressors
    // just for this would be a lot of dependency for one call.
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(dir)
        .status()
        .context("running tar")?;
    let _ = fs::remove_file(&archive);

    if !status.success() {
        bail!("tar failed to unpack {}", release.name);
    }
    Ok(())
}

/// Streams a file to disk, reporting (downloaded, total). `expected` stands in
/// when the server sends no content-length; 0 means "unknown".
pub(crate) fn download(
    url: &str,
    target: &Path,
    expected: u64,
    on_progress: &dyn Fn(u64, u64),
) -> Result<()> {
    let response = http::agent(Duration::from_secs(60 * 60))
        .get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?;

    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(expected);

    let mut reader = response.into_body().into_reader();
    let mut file =
        File::create(target).with_context(|| format!("creating {}", target.display()))?;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut done = 0u64;

    loop {
        let read = reader.read(&mut buffer).context("reading the download")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .context("writing the download")?;
        done += read as u64;
        on_progress(done, total);
    }
    file.flush()?;
    Ok(())
}

/// Deletes an installed build. Only builds Divine put there can go: anything
/// else belongs to Steam or the distribution, and removing it would break them.
pub fn remove(path: &Path, managed_dir: &Path) -> Result<()> {
    if !path.starts_with(managed_dir) || path == managed_dir {
        bail!("{} was not installed by Divine", path.display());
    }
    if !path.join("proton").is_file() {
        bail!("{} does not look like a Proton build", path.display());
    }
    fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Release assets carry the architecture in the file name, except the old
    /// ones from before there was more than one architecture to carry.
    #[test]
    fn only_the_tarballs_for_this_machine_are_offered() {
        assert!(built_for("GE-Proton10-1.tar.gz", "x86_64"));
        assert!(built_for("GE-Proton10-1-x86_64.tar.xz", "x86_64"));
        // An arm build is not for an x86 machine, and the reverse.
        assert!(!built_for("GE-Proton10-1-aarch64.tar.gz", "x86_64"));
        assert!(built_for("GE-Proton10-1-arm64.tar.gz", "aarch64"));
        assert!(!built_for("GE-Proton10-1-x86_64.tar.gz", "aarch64"));
        // Untagged predates multi-arch, so it is x86_64 and nothing else.
        assert!(!built_for("GE-Proton10-1.tar.gz", "aarch64"));

        // Whatever else a release ships is not a Proton build.
        assert!(!built_for("GE-Proton10-1.sha512sum", "x86_64"));
        assert!(!built_for("Source code (zip)", "x86_64"));
        assert!(!built_for("GE-Proton10-1.tar.gz", "riscv64"));
    }

    #[test]
    fn a_build_is_named_by_its_tarball_without_the_extension() {
        assert_eq!(strip_extension("GE-Proton10-1.tar.gz"), "GE-Proton10-1");
        assert_eq!(
            strip_extension("proton-cachyos-10.0.tar.xz"),
            "proton-cachyos-10.0"
        );
        assert_eq!(strip_extension("GE-Proton10-1"), "GE-Proton10-1");
    }
}
