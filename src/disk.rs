//! Free space, which disk a path is on, moving a folder between them, and
//! noticing drives come and go.
//!
//! Two different questions get asked here and they have different answers on
//! btrfs, which is worth spelling out because getting them the wrong way round
//! produces nonsense.
//!
//! `stat` hands every btrfs subvolume its own anonymous device, so two
//! subvolumes of one filesystem look like separate disks: `rename` between them
//! fails with EXDEV, which is exactly why `legendary move` gives up. Yet they
//! share one pool of free space, and copying between them is a reflink — the
//! bytes are not written twice.
//!
//! So the two questions come apart. Whether a rename can work is answered by
//! trying one, since guessing buys nothing. Whether the move costs any space is
//! [`same_disk`], which reads the real device out of `/proc/self/mountinfo`
//! rather than trusting `st_dev`.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// How a copy ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Copied {
    Done,
    Cancelled,
}

/// A message each time something is mounted or unmounted anywhere on the system.
///
/// The kernel flags an open `/proc/self/mountinfo` with POLLPRI on every change
/// to the mount table, and the poll that reports it clears the flag again, so
/// one change wakes this once. GIO's mount monitor rests on the same signal,
/// but glib-rs keeps file descriptor sources in a crate of its own; a thread
/// asleep in `poll` costs nothing and needs no new dependency.
pub fn mount_changes() -> Result<async_channel::Receiver<()>> {
    let table = fs::File::open("/proc/self/mountinfo").context("opening /proc/self/mountinfo")?;
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("mount-watch".into())
        .spawn(move || {
            let mut watch = libc::pollfd {
                fd: table.as_raw_fd(),
                events: libc::POLLPRI,
                revents: 0,
            };
            loop {
                // SAFETY: one pollfd, alive for the whole call, and the file it
                // names is owned by this thread.
                if unsafe { libc::poll(&mut watch, 1, -1) } < 0 {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                if watch.revents & libc::POLLNVAL != 0 || tx.is_closed() {
                    return;
                }
                // A full channel already has a look pending, and one look
                // covers any number of changes.
                let _ = tx.try_send(());
            }
        })
        .context("starting the mount watcher")?;
    Ok(rx)
}

/// Bytes free on the filesystem holding `path`, or the nearest parent that
/// exists — the folder being installed into is usually not there yet.
pub fn free_space(path: &Path) -> Option<u64> {
    let existing = nearest_existing(path)?;
    let name = std::ffi::CString::new(existing.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(name.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    // f_bavail rather than f_bfree: the blocks held back for root are not ours.
    Some(stats.f_bavail as u64 * stats.f_frsize as u64)
}

/// Whether both paths draw on the same pool of free space, which is what
/// somebody means by "the same disk".
pub fn same_disk(one: &Path, other: &Path) -> bool {
    match (device_of(one), device_of(other)) {
        (Some(one), Some(other)) => one == other,
        // Not knowing costs a line of text that did not need showing, which is
        // the better way to be wrong.
        _ => false,
    }
}

/// The mount's own major:minor, straight out of `/proc/self/mountinfo`.
///
/// This is the real device, unlike `st_dev`: every btrfs subvolume gets an
/// anonymous one of those, while all of them report the same major:minor here.
fn device_of(path: &Path) -> Option<String> {
    let wanted = nearest_existing(path)?.canonicalize().ok()?;
    let table = fs::read_to_string("/proc/self/mountinfo").ok()?;

    // Mount points nest, so the longest one that still fits is the answer.
    let mut best: Option<(usize, String)> = None;
    for line in table.lines() {
        let mut fields = line.split_whitespace();
        let device = fields.nth(2)?.to_string();
        let point = PathBuf::from(unescape(fields.nth(1)?));
        if !wanted.starts_with(&point) {
            continue;
        }
        let depth = point.components().count();
        if best.as_ref().is_none_or(|(deepest, _)| depth > *deepest) {
            best = Some((depth, device));
        }
    }
    best.map(|(_, device)| device)
}

/// mountinfo writes the awkward characters in a path as octal escapes.
fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut rest = field;
    while let Some(cut) = rest.find('\\') {
        out.push_str(&rest[..cut]);
        let escape = &rest[cut + 1..];
        match escape
            .get(..3)
            .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        {
            Some(byte) => {
                out.push(byte as char);
                rest = &escape[3..];
            }
            None => {
                out.push('\\');
                rest = escape;
            }
        }
    }
    out.push_str(rest);
    out
}

fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
    }
}

/// Bytes a directory takes up. Symlinks are counted by their own size and
/// never followed, so nothing outside is added in or copied later.
pub fn size_on_disk(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => size_on_disk(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|data| data.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// Copies a directory across, reporting bytes as they land.
///
/// `fs::copy` goes through `copy_file_range`, which btrfs answers with a
/// reflink where it can: between two subvolumes of one filesystem this moves
/// gigabytes instantly and writes nothing. Elsewhere it is an ordinary copy.
pub fn copy_tree(
    from: &Path,
    to: &Path,
    stopped: &dyn Fn() -> bool,
    on_progress: &dyn Fn(u64),
) -> Result<Copied> {
    let done = AtomicU64::new(0);
    copy_into(from, to, stopped, on_progress, &done)
}

fn copy_into(
    from: &Path,
    to: &Path,
    stopped: &dyn Fn() -> bool,
    on_progress: &dyn Fn(u64),
    done: &AtomicU64,
) -> Result<Copied> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    let entries = fs::read_dir(from).with_context(|| format!("reading {}", from.display()))?;

    for entry in entries {
        if stopped() {
            return Ok(Copied::Cancelled);
        }
        let entry = entry.context("reading a directory entry")?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().context("looking at a directory entry")?;

        if kind.is_dir() {
            if copy_into(&source, &target, stopped, on_progress, done)? == Copied::Cancelled {
                return Ok(Copied::Cancelled);
            }
        } else if kind.is_symlink() {
            // Carried over as a link. Following it would copy whatever it
            // points at into the game folder, which is not what was there.
            let aim = fs::read_link(&source)
                .with_context(|| format!("reading the link {}", source.display()))?;
            let _ = fs::remove_file(&target);
            std::os::unix::fs::symlink(&aim, &target)
                .with_context(|| format!("linking {}", target.display()))?;
        } else {
            let copied = fs::copy(&source, &target)
                .with_context(|| format!("copying {}", source.display()))?;
            on_progress(done.fetch_add(copied, Ordering::Relaxed) + copied);
        }
    }
    Ok(Copied::Done)
}

/// Removes a directory, saying nothing if it was already gone.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mount_point_with_awkward_characters_is_unescaped() {
        assert_eq!(unescape("/mnt/my\\040games"), "/mnt/my games");
        assert_eq!(unescape("/Games"), "/Games");
        // A lone backslash is not an escape and stays put.
        assert_eq!(unescape("/od\\d"), "/od\\d");
    }

    /// The folder being installed into does not exist yet, so the question has
    /// to be answered by the nearest parent that does.
    #[test]
    fn a_folder_that_is_not_there_yet_is_answered_by_its_parent() {
        assert_eq!(nearest_existing(Path::new("/")), Some(PathBuf::from("/")));
        assert_eq!(
            nearest_existing(Path::new("/this/does/not/exist/anywhere")),
            Some(PathBuf::from("/"))
        );

        let here = std::env::temp_dir();
        assert_eq!(nearest_existing(&here.join("no/such/game")), Some(here));

        assert!(free_space(Path::new("/this/does/not/exist")).is_some_and(|free| free > 0));
    }

    /// The question this answers is whether a move costs any space, so two
    /// paths on genuinely different filesystems have to come out different.
    #[test]
    fn separate_filesystems_are_separate_disks() {
        assert!(same_disk(Path::new("/"), Path::new("/")));
        assert!(device_of(Path::new("/")).is_some());

        // /proc is its own filesystem on every Linux, mounted over nothing.
        assert!(!same_disk(Path::new("/"), Path::new("/proc")));

        // Not knowing is reported as "different", which only costs a line of
        // text that did not need showing.
        assert!(!same_disk(Path::new("/"), Path::new("\0not a path")));
    }

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("divine-{name}-{}", std::process::id()));
        let _ = remove_tree(&path);
        path
    }

    /// A game folder in miniature: a file, a nested one, and a symlink, which
    /// some games really do ship.
    fn plant(root: &Path) {
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join("game.exe"), vec![b'x'; 4096]).unwrap();
        fs::write(root.join("data").join("pak"), vec![b'y'; 2048]).unwrap();
        std::os::unix::fs::symlink("game.exe", root.join("link")).unwrap();
    }

    #[test]
    fn a_tree_is_copied_whole() {
        let from = scratch("copy-from");
        let to = scratch("copy-to");
        plant(&from);

        let seen = AtomicU64::new(0);
        let outcome = copy_tree(&from, &to, &|| false, &|done| {
            seen.store(done, Ordering::Relaxed)
        })
        .unwrap();

        assert_eq!(outcome, Copied::Done);
        assert_eq!(fs::read(to.join("game.exe")).unwrap().len(), 4096);
        assert_eq!(fs::read(to.join("data").join("pak")).unwrap().len(), 2048);
        // Carried over as a link, not as a second copy of what it points at.
        assert!(fs::symlink_metadata(to.join("link")).unwrap().is_symlink());
        assert_eq!(
            fs::read_link(to.join("link")).unwrap(),
            Path::new("game.exe")
        );
        assert_eq!(seen.load(Ordering::Relaxed), 4096 + 2048);
        assert_eq!(size_on_disk(&from), size_on_disk(&to));

        remove_tree(&from).unwrap();
        remove_tree(&to).unwrap();
    }

    #[test]
    fn a_cancelled_copy_stops_and_says_so() {
        let from = scratch("cancel-from");
        let to = scratch("cancel-to");
        plant(&from);

        let outcome = copy_tree(&from, &to, &|| true, &|_| {}).unwrap();

        assert_eq!(outcome, Copied::Cancelled);
        // Whatever landed is the caller's to clear; the source is untouched.
        assert_eq!(size_on_disk(&from), 4096 + 2048);

        remove_tree(&from).unwrap();
        remove_tree(&to).unwrap();
    }

    #[test]
    fn removing_something_already_gone_is_not_an_error() {
        assert!(remove_tree(&scratch("never-there")).is_ok());
    }
}
