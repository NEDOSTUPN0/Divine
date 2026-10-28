//! What Divine has to say, and what the programs it runs had to say.
//!
//! Every message becomes a [`Record`] before it becomes text: when it happened,
//! how bad it is, and who said it. Divine runs legendary, which runs umu, which
//! runs the game, and when something goes wrong the first question is always
//! which of them is complaining — so the source is part of the record rather
//! than something spelled into the message and lost.
//!
//! Records go three places. They are written to stderr as they arrive, which is
//! where a terminal or the journal picks them up; they are kept in a bounded
//! [`history`] so that a window can show them without having been open when
//! they happened; and, once [`to_file`] has been called, they are appended to a
//! file that outlives the process. Nothing in this module knows about GTK: a
//! viewer asks for the history when it opens and installs a [`listen`]er for
//! what comes after, and marshalling that onto the main thread is the viewer's
//! business.
//!
//! The stderr copy carries no timestamp on purpose. The journal stamps every
//! line it takes, and a second copy in the middle of the message helps nobody;
//! the [`Record`] keeps the real time for whoever wants to display it. The file
//! is read long after the fact by somebody who has no journal to line it up
//! against, so that copy is stamped.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use gtk::glib;

/// How much history a viewer can still find. A long session with a chatty
/// download is a few thousand lines, and this is bounded because the process
/// stays up for days.
const KEPT: usize = 4000;

/// How bad a message is. Ordered, so a threshold is a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// The word that appears in the line, padded so messages line up.
    fn label(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info ",
            Level::Warn => "warn ",
            Level::Error => "error",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" | "critical" => Some(Level::Error),
            _ => None,
        }
    }
}

/// Who said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Divine,
    /// A line relayed from a program Divine ran, named as the user knows it.
    Tool(&'static str),
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Divine => "divine",
            Source::Tool(name) => name,
        }
    }
}

/// One thing that happened.
#[derive(Debug, Clone)]
pub struct Record {
    pub at: SystemTime,
    pub level: Level,
    pub source: Source,
    pub message: String,
}

/// The lowest level that gets through, from `DIVINE_LOG`. Read once: this is
/// asked on every message, and the environment does not change under us.
fn threshold() -> Level {
    static THRESHOLD: OnceLock<Level> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("DIVINE_LOG")
            .ok()
            .and_then(|name| Level::parse(&name))
            .unwrap_or(Level::Info)
    })
}

fn file() -> &'static Mutex<Option<File>> {
    static FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
    FILE.get_or_init(|| Mutex::new(None))
}

/// Starts copying every record into `path`. Call once, before anything worth
/// reading has happened.
///
/// The previous session moves aside instead of being overwritten: a launch
/// that failed is usually looked at after the launcher has been restarted, and
/// that restart would otherwise take the evidence with it.
pub fn to_file(path: &Path) -> Result<()> {
    if path.exists() {
        let mut previous = PathBuf::from(path).into_os_string();
        previous.push(".1");
        let previous = PathBuf::from(previous);
        fs::rename(path, &previous)
            .with_context(|| format!("moving {} aside", previous.display()))?;
    }
    let opened = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    if let Ok(mut file) = file().lock() {
        *file = Some(opened);
    }
    Ok(())
}

/// `2026-08-21 21:09:44` in the user's own time, which is the clock they will
/// compare the log against.
fn stamp(at: SystemTime) -> String {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default();
    glib::DateTime::from_unix_local(seconds)
        .and_then(|at| at.format("%Y-%m-%d %H:%M:%S"))
        .map(|at| at.to_string())
        .unwrap_or_default()
}

fn kept() -> &'static Mutex<VecDeque<Record>> {
    static KEPT_RECORDS: OnceLock<Mutex<VecDeque<Record>>> = OnceLock::new();
    KEPT_RECORDS.get_or_init(|| Mutex::new(VecDeque::with_capacity(KEPT)))
}

/// Returns false once it has nothing left to do with the records, which is how
/// a window that has been closed stops being told about them.
type Listener = Box<dyn Fn(&Record) -> bool + Send + Sync>;

/// Numbered so one can be dropped without disturbing the rest, whichever
/// thread happens to be writing a record at the time.
fn listeners() -> &'static RwLock<Vec<(u64, Listener)>> {
    static LISTENERS: OnceLock<RwLock<Vec<(u64, Listener)>>> = OnceLock::new();
    LISTENERS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Writes a record down. The macros below are the usual way in; this is public
/// for [`from_tool`] and for anything else that has a source of its own.
pub fn record(level: Level, source: Source, message: String) {
    // The threshold is the terminal's, not the record's. Everything is kept
    // and handed to whoever is listening, so a window can show the debug lines
    // of a session that was not started with DIVINE_LOG set.
    if level >= threshold() {
        eprintln!("{}: {}: {}", source.name(), level.label().trim(), message);
    }

    let entry = Record {
        at: SystemTime::now(),
        level,
        source,
        message,
    };
    if let Ok(mut kept) = kept().lock() {
        if kept.len() == KEPT {
            kept.pop_front();
        }
        kept.push_back(entry.clone());
    }
    // The file takes everything, whatever the terminal was told to show: the
    // session it is written for is over by the time anybody thinks to set
    // DIVINE_LOG. A failed write is dropped rather than logged, since logging
    // it would come straight back here.
    if let Ok(mut file) = file().lock() {
        if let Some(file) = file.as_mut() {
            let _ = writeln!(
                file,
                "{} {}: {}: {}",
                stamp(entry.at),
                entry.source.name(),
                entry.level.label().trim(),
                entry.message
            );
        }
    }
    // Called with no lock of ours held, so a listener is free to log.
    let mut finished = Vec::new();
    if let Ok(listeners) = listeners().read() {
        for (id, listener) in listeners.iter() {
            if !listener(&entry) {
                finished.push(*id);
            }
        }
    }
    if !finished.is_empty() {
        if let Ok(mut listeners) = listeners().write() {
            listeners.retain(|(id, _)| !finished.contains(id));
        }
    }
}

/// Relays one line a program Divine ran wrote to its stderr.
///
/// legendary formats its own lines as `[cli] INFO: message`, and taking it at
/// its word is the difference between a viewer that can colour its warnings and
/// one that shows a wall of grey. A line in any other shape is kept whole and
/// called information, since the program clearly meant to say it.
pub fn from_tool(tool: &'static str, line: &str) {
    let (level, message) = split_level(line).unwrap_or((Level::Info, line));
    record(level, Source::Tool(tool), message.trim().to_string());
}

/// Relays one line from a game's launch, where several programs share a stream.
///
/// Divine runs legendary, legendary runs umu, umu runs Proton and the game, and
/// all of it comes back down one pipe. Each writes in a shape of its own, and
/// these are the shapes — every one of them copied from a real launch, or from
/// the program's own source:
///
/// - `[cli] INFO: Launching …` — legendary, whose `basicConfig` format is
///   `[%(name)s] %(levelname)s: %(message)s`.
/// - `INFO: umu-launcher version 1.4.4` — umu, whose `umu_log.py` formats as
///   `%(levelname)s: %(message)s`, or `[umu.umu_run:57] DEBUG: …` when it is
///   asked for debug.
/// - `ProtonFixes[281324] INFO: Running checks` — protonfixes, which umu ships.
/// - `Proton: Upgrading prefix …`, `pressure-vessel-wrap[281174]: W: …`,
///   `ntsync: up and running.` — the Proton stack.
///
/// Anything in no shape at all is the game talking, which is the useful thing
/// to be able to pick out of all this.
pub fn from_launch(line: &str) {
    let (tool, level, message) = attribute(line.trim_start());
    record(level, Source::Tool(tool), message.trim().to_string());
}

fn attribute(line: &str) -> (&'static str, Level, &str) {
    // Both legendary and umu open with a bracketed tag. legendary's is the
    // name of one of its own modules; umu's starts with its own name.
    if let Some((tag, rest)) = line.strip_prefix('[').and_then(|s| s.split_once(']')) {
        let tool = match tag.starts_with("umu") {
            true => "umu",
            false => "legendary",
        };
        let (level, message) = split_level(rest).unwrap_or((Level::Info, rest));
        return (tool, level, message);
    }

    if let Some(rest) = line.strip_prefix("ProtonFixes") {
        let rest = rest.split_once(']').map(|(_, rest)| rest).unwrap_or(rest);
        let (level, message) = split_level(rest).unwrap_or((Level::Info, rest));
        return ("umu", level, message);
    }

    if let Some(rest) = line.strip_prefix("Proton:") {
        // Proton says "Error:" outright when it means it; everything else it
        // writes is a remark about the prefix.
        let level = match rest.trim_start().starts_with("Error") {
            true => Level::Error,
            false => Level::Info,
        };
        return ("proton", level, rest);
    }

    if line.starts_with("pressure-vessel") {
        // Its levels are single letters. W and E are the ones a launch shows;
        // anything else is it narrating.
        let rest = line.split_once(']').map(|(_, rest)| rest).unwrap_or(line);
        let rest = rest.trim_start_matches(':').trim_start();
        return match rest.split_once(": ") {
            Some(("W", message)) => ("proton", Level::Warn, message),
            Some(("E", message)) => ("proton", Level::Error, message),
            _ => ("proton", Level::Info, rest),
        };
    }

    // Wine's own, from underneath Proton.
    if ["wine:", "ntsync:", "err:", "fixme:", "warn:"]
        .iter()
        .any(|start| line.starts_with(start))
    {
        return ("proton", Level::Info, line);
    }

    // umu with nothing in front of the level is its ordinary format.
    match split_level(line) {
        Some((level, message)) => ("umu", level, message),
        None => ("game", Level::Info, line),
    }
}

/// Splits `[cli] INFO: message` into the level and what is left.
fn split_level(line: &str) -> Option<(Level, &str)> {
    let rest = match line.trim_start().strip_prefix('[') {
        Some(tagged) => tagged.split_once(']')?.1,
        None => line,
    };
    let (word, message) = rest.split_once(':')?;
    Some((Level::parse(word)?, message))
}

/// Everything still kept, oldest first — what a viewer shows when it opens.
pub fn history() -> Vec<Record> {
    kept()
        .lock()
        .map(|kept| kept.iter().cloned().collect())
        .unwrap_or_default()
}

/// Calls `listener` for every record from now on, from whichever thread wrote
/// it, until it returns false. A viewer that wants to touch widgets has to hop
/// to the main thread itself.
pub fn listen(listener: impl Fn(&Record) -> bool + Send + Sync + 'static) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Ok(mut listeners) = listeners().write() {
        listeners.push((NEXT.fetch_add(1, Ordering::Relaxed), Box::new(listener)));
    }
}

// Exported from the crate root under prefixed names, then re-exported here
// under the ones worth typing. The detour is for `warn`, which is also a
// built-in attribute and ambiguous with a plain `macro_rules!` of that name.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::log::record($crate::log::Level::Debug, $crate::log::Source::Divine, format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::record($crate::log::Level::Info, $crate::log::Source::Divine, format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::log::record($crate::log::Level::Warn, $crate::log::Source::Divine, format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::log::record($crate::log::Level::Error, $crate::log::Source::Divine, format!($($arg)*))
    };
}

pub(crate) use crate::log_debug as debug;
pub(crate) use crate::log_error as error;
pub(crate) use crate::log_info as info;
pub(crate) use crate::log_warn as warn;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legendary_lines_keep_the_level_they_were_written_with() {
        // Straight out of legendary's own basicConfig format.
        assert_eq!(
            split_level("[cli] INFO: Launching Fowl..."),
            Some((Level::Info, " Launching Fowl..."))
        );
        assert_eq!(
            split_level("[DLManager] WARNING: Chunk download failed"),
            Some((Level::Warn, " Chunk download failed"))
        );
        assert_eq!(
            split_level("[Core] ERROR: Login failed"),
            Some((Level::Error, " Login failed"))
        );
    }

    /// Progress bars and tracebacks carry no level, and mangling them into one
    /// would lose the line.
    #[test]
    fn a_line_in_any_other_shape_is_left_alone() {
        assert_eq!(
            split_level(" = Progress: 42.0% (1/2), Running for 00:10"),
            None
        );
        assert_eq!(split_level("Traceback (most recent call last)"), None);
        assert_eq!(split_level(""), None);
    }

    /// End to end: a relayed line keeps its level and its source, lands in the
    /// history a viewer would read, and reaches a listener that asked for it.
    #[test]
    fn a_relayed_line_arrives_whole() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let heard = std::sync::Arc::clone(&seen);
        listen(move |record| {
            if record.message.contains("wren") {
                if let Ok(mut heard) = heard.lock() {
                    heard.push(record.clone());
                }
            }
            true
        });

        from_tool("legendary", "[DLManager] WARNING: wren download failed");

        let mine = |record: &&Record| record.message.contains("wren");
        let kept = history();
        let kept = kept.iter().find(mine).expect("not in the history");
        assert_eq!(kept.level, Level::Warn);
        assert_eq!(kept.source, Source::Tool("legendary"));
        // Named by the tool, not by the format it wrote the line in.
        assert_eq!(kept.message, "wren download failed");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    /// Every one of these came off a real launch of Among Us, in this order.
    /// Telling them apart is the whole point of the log window: the question
    /// after a failed launch is which of the four is complaining.
    #[test]
    fn a_launch_is_sorted_by_who_is_talking() {
        let who = |line| {
            let (tool, level, message) = attribute(line);
            (tool, level, message.trim().to_string())
        };

        assert_eq!(
            who("[cli] INFO: Launching 963137e4c29d4c79a81323b8fab03a40..."),
            (
                "legendary",
                Level::Info,
                "Launching 963137e4c29d4c79a81323b8fab03a40...".to_string()
            )
        );
        assert_eq!(
            who("INFO: umu-launcher version 1.4.4 (3.14.7)"),
            (
                "umu",
                Level::Info,
                "umu-launcher version 1.4.4 (3.14.7)".into()
            )
        );
        assert_eq!(
            who("[umu.umu_run:57] DEBUG: Checking for updates"),
            ("umu", Level::Debug, "Checking for updates".into())
        );
        assert_eq!(
            who("ProtonFixes[281324] WARN: Game title not found in CSV"),
            ("umu", Level::Warn, "Game title not found in CSV".into())
        );
        assert_eq!(
            who("Proton: Upgrading prefix from GE-Proton10-34 to GE-Proton11-5"),
            (
                "proton",
                Level::Info,
                "Upgrading prefix from GE-Proton10-34 to GE-Proton11-5".into()
            )
        );
        assert_eq!(
            who("Proton: Error: unable to use parent for game drive, path /Games"),
            (
                "proton",
                Level::Error,
                "Error: unable to use parent for game drive, path /Games".into()
            )
        );
        assert_eq!(
            who(r#"pressure-vessel-wrap[281174]: W: "run/opengl-driver" is unlikely to appear"#),
            (
                "proton",
                Level::Warn,
                r#""run/opengl-driver" is unlikely to appear"#.into()
            )
        );
        assert_eq!(
            who("ntsync: up and running."),
            ("proton", Level::Info, "ntsync: up and running.".into())
        );
        // Nothing recognisable is the game itself, which is what somebody
        // reading this log is usually after.
        assert_eq!(
            who("reg: The operation completed successfully"),
            (
                "game",
                Level::Info,
                "reg: The operation completed successfully".into()
            )
        );
    }

    #[test]
    fn a_session_is_written_down_and_the_one_before_it_kept() {
        let dir = std::env::temp_dir().join(format!("divine-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("divine.log");

        to_file(&path).unwrap();
        record(Level::Debug, Source::Tool("umu"), "first session".into());
        to_file(&path).unwrap();
        record(Level::Debug, Source::Divine, "second session".into());

        let now = fs::read_to_string(&path).unwrap();
        let before = fs::read_to_string(dir.join("divine.log.1")).unwrap();
        assert!(now.contains("divine: debug: second session"), "{now}");
        assert!(before.contains("umu: debug: first session"), "{before}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn levels_are_ordered_worst_last() {
        assert!(Level::Error > Level::Warn);
        assert!(Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug);
    }
}
