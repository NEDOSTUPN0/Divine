//! Cover art: pick a size, download once, keep it on disk.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config;
use crate::http;
use crate::models::Cover;

/// Epic serves 1200x1600 originals; the grid never needs more than this.
const COVER_QUERY: &str = "?h=480&w=360&resize=1";
const MAX_BYTES: u64 = 8 * 1024 * 1024;

pub fn path_for(cover: &Cover) -> PathBuf {
    config::covers_dir().join(format!("{}.img", cover.md5))
}

/// Some Epic cover URLs contain raw spaces (e.g. "...Portrait Store Banner..."),
/// which is not a valid URI and gets rejected before the request is made. Encode
/// the characters that are outright illegal, and only those, so already encoded
/// URLs are left alone.
fn escape_url(url: &str) -> String {
    const ILLEGAL: [char; 9] = [' ', '"', '<', '>', '\\', '^', '`', '{', '}'];
    let mut out = String::with_capacity(url.len());
    for ch in url.chars() {
        if ILLEGAL.contains(&ch) || ch.is_control() {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).as_bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// What a screenshot is filed under. The store gives no hash to key them by,
/// so the URL is it. Shared with the cache sweep, which has to name the same
/// file from the other direction.
pub fn screenshot_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn screenshots_dir() -> PathBuf {
    config::cache_dir().join("screenshots")
}

pub fn achievements_dir() -> PathBuf {
    config::cache_dir().join("achievements")
}

/// An achievement's icon. Filed by URL like the screenshots, since Epic gives
/// these no hash either.
///
/// Asked for exactly as it comes: the achievement CDN answers 403 to the resize
/// parameters the cover one accepts, and there is nothing to gain by asking
/// anyway, since Epic already serves these at 128 square.
pub fn fetch_achievement(url: &str) -> Result<PathBuf> {
    let dir = achievements_dir();
    let path = dir.join(format!("{}.img", screenshot_name(url)));
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(&dir)?;
    download(&escape_url(url), &path)?;
    Ok(path)
}

/// Store screenshots, kept beside the covers.
pub fn fetch_screenshot(url: &str) -> Result<PathBuf> {
    let dir = screenshots_dir();
    let path = dir.join(format!("{}.img", screenshot_name(url)));
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(&dir)?;
    // Originals are 1920x1080; the page shows them at a fraction of that.
    download(&format!("{}?h=360&w=640&resize=1", escape_url(url)), &path)?;
    Ok(path)
}

/// Returns the cached file, downloading it first if needed. Blocking on purpose:
/// callers run it on a worker thread.
pub fn fetch(cover: &Cover) -> Result<PathBuf> {
    let path = path_for(cover);
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(config::covers_dir())?;
    download(&format!("{}{COVER_QUERY}", escape_url(&cover.url)), &path)?;
    Ok(path)
}

fn download(url: &str, path: &std::path::Path) -> Result<()> {
    let mut body = http::agent(Duration::from_secs(30))
        .get(url)
        .call()
        .with_context(|| format!("fetching {url}"))?
        .into_body();

    let mut bytes = Vec::new();
    body.as_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .context("reading image")?;

    // Write beside the target first so a killed download never leaves a
    // truncated file in the cache.
    let tmp = path.with_extension("part");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Epic's image URLs carry spaces and the odd bracket, which a request line
    /// cannot. Everything legal is left exactly as it was.
    #[test]
    fn only_the_characters_a_url_cannot_hold_are_escaped() {
        assert_eq!(
            escape_url("https://cdn.epic/Fowl/The Escapists 2.png"),
            "https://cdn.epic/Fowl/The%20Escapists%202.png"
        );
        assert_eq!(
            escape_url("https://cdn.epic/a?h=128&w=128&resize=1"),
            "https://cdn.epic/a?h=128&w=128&resize=1"
        );
        // Only the characters a URL genuinely cannot hold are touched. Letters
        // outside ASCII are legal in a URL and go through as they are, which is
        // also what a browser does with them.
        assert_eq!(
            escape_url("https://cdn.epic/{я}"),
            "https://cdn.epic/%7Bя%7D"
        );
        // Control characters cannot travel in a request line at all.
        assert_eq!(
            escape_url("https://cdn.epic/a\nb"),
            "https://cdn.epic/a%0Ab"
        );
    }

    /// The contract cleanup leans on: it works out which screenshots are still
    /// wanted by naming the URLs on the store page the same way the downloader
    /// named the files. If these two ever disagreed, live art would be swept.
    #[test]
    fn a_screenshot_is_named_after_its_url_and_nothing_else() {
        let url = "https://cdn.epic/Fowl/shot1.png";
        assert_eq!(screenshot_name(url), screenshot_name(url));
        assert_ne!(
            screenshot_name(url),
            screenshot_name("https://cdn.epic/Fowl/shot2.png")
        );

        // A fixed-width hex name, so it is a filename on any filesystem.
        let name = screenshot_name(url);
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
