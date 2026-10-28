//! The Epic Games Launcher wrapper, and how it gets onto the machine.
//!
//! Some games ask their parent process who it is and sell themselves to you
//! again if the answer is not Epic's launcher. The wrapper in
//! `vendor/heroic-epic-integration` answers for them; it is a Windows binary,
//! because it runs inside the prefix rather than on the host.
//!
//! Building it needs a mingw cross compiler, which every distribution that
//! packages Divine has and a Flatpak SDK does not. So a build that could not
//! compile one fetches the same binary from Divine's own releases instead, and
//! the rest of Divine never has to know which of the two happened.
//!
//! The download is checked against a hash written down here rather than trusted
//! because it arrived over TLS: this is an executable that will be run inside
//! the player's prefix, and `nix build .#epic-shim` reproduces it byte for byte,
//! which is what makes one constant enough to pin it.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::config;
use crate::http;
use crate::log::info;

/// What the file is called wherever it sits, host side or in the prefix.
pub const EXE: &str = "EpicGamesLauncher.exe";

/// Built with `nix build .#epic-shim` and attached to that release by hand.
const URL: &str =
    "https://github.com/NEDOSTUPN0/Divine/releases/download/epic-shim-1.0.0/EpicGamesLauncher.exe";

/// SHA-256 of `nix build .#epic-shim`. Changing the wrapper means building it,
/// publishing it and changing this line together.
const SHA256: &str = "e52a3143deca83786686b2c0ea200965c26f31dc1206bb41408b7edfe614709f";

/// The wrapper is 81 KB. The ceiling is only here so that a redirect to
/// something else entirely is not read into memory in full before it is
/// rejected.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The wrapper, if this machine already has one. Checks what the build shipped
/// before what a previous run downloaded, so a packaged copy always wins.
pub fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DIVINE_EPIC_SHIM") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    installed_dirs()
        .into_iter()
        .map(|dir| dir.join("divine").join(EXE))
        .chain(std::iter::once(config::shim_file()))
        .find(|path| path.is_file())
}

/// The wrapper, downloading it first if this build did not ship one.
///
/// Blocking, and only ever called for a game whose settings ask for the
/// wrapper: a library that never touches a Rockstar title never fetches this.
pub fn ensure() -> Result<PathBuf> {
    if let Some(path) = path() {
        return Ok(path);
    }
    download()
}

fn download() -> Result<PathBuf> {
    info!("fetching the Epic Games Launcher wrapper");
    let response = http::agent(Duration::from_secs(60))
        .get(URL)
        .call()
        .with_context(|| format!("downloading {URL}"))?;

    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .context("reading the Epic Games Launcher wrapper")?;

    let got = sha256(&bytes);
    if got != SHA256 {
        bail!(
            "the Epic Games Launcher wrapper does not match its hash: expected {SHA256}, got {got}"
        );
    }

    let target = config::shim_file();
    let dir = target.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // Written beside the target and renamed, so an interrupted download cannot
    // leave a half a wrapper behind for the next launch to find and trust.
    let partial = target.with_extension("part");
    fs::write(&partial, &bytes).with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, &target).with_context(|| format!("saving {}", target.display()))?;
    Ok(target)
}

/// Data directories a distribution may have installed Divine's own files into.
fn installed_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent().and_then(Path::parent) {
            dirs.push(parent.join("share"));
        }
    }
    let xdg = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    dirs.extend(xdg.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs.push(PathBuf::from("/usr/local/share"));
    dirs.push(PathBuf::from("/usr/share"));
    dirs
}

/// SHA-256, as FIPS 180-4 spells it. One call site and eighty lines beats a
/// dependency on a crate that would bring its own traits and its own
/// generics for the same answer.
fn sha256(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = bytes.to_vec();
    let bits = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (slot, word) in w.iter_mut().zip(block.chunks_exact(4)) {
            *slot = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut s] = h;
        for (k, word) in K.iter().zip(w.iter()) {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = s
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(*k)
                .wrapping_add(*word);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = sum0.wrapping_add(majority);

            s = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, s]) {
            *slot = slot.wrapping_add(value);
        }
    }

    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hashes_are_the_ones_fips_180_4_prints() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// A block boundary is where a padding mistake hides: 55 bytes is the last
    /// length that still fits its length field, 56 is the first that does not.
    #[test]
    fn a_message_that_fills_its_block_is_padded_into_the_next_one() {
        assert_eq!(
            sha256(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
    }
}
