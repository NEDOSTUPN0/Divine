//! Games Divine started that are still up, remembered across a restart.
//!
//! A launch gets a process group of its own so that stopping it later can
//! signal umu, Proton and the game together. The side of that which matters
//! here is that the group outlives Divine: closing the launcher leaves the game
//! running. Divine's own note of it did not survive, so a restart offered Play
//! for a game already playing, and the play session left open in the journal
//! stayed open until something else closed it.
//!
//! What is written down is the group and when its leader started. The group id
//! alone would not do: process ids come round again, and a launcher offering to
//! stop whatever inherited the number would kill something it never started.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One game still running, as far as this machine can tell.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Live {
    pub app_name: String,
    /// The process group, which is also the leader's pid.
    pub group: i32,
    /// The leader's start time in clock ticks since boot, straight out of
    /// `/proc`. Together with the pid it names one particular process.
    pub since: u64,
}

/// Writes down a game that has just started.
pub fn remember(app_name: &str, group: i32) {
    let Some(since) = started_at(group) else {
        return;
    };
    let mut live: Vec<Live> = kept()
        .into_iter()
        .filter(|other| other.app_name != app_name)
        .collect();
    live.push(Live {
        app_name: app_name.to_string(),
        group,
        since,
    });
    save(&live);
}

/// Drops a game Divine has seen end.
pub fn forget(app_name: &str) {
    let live: Vec<Live> = kept()
        .into_iter()
        .filter(|other| other.app_name != app_name)
        .collect();
    save(&live);
}

/// What is still running of what was written down, the rest dropped from the
/// file on the way past.
pub fn alive() -> Vec<Live> {
    let (alive, gone): (Vec<Live>, Vec<Live>) = kept().into_iter().partition(is_alive);
    if !gone.is_empty() {
        save(&alive);
    }
    alive
}

/// Whether that exact process is still there. A group whose leader has gone and
/// whose number has been handed to something else fails the second half.
pub fn is_alive(live: &Live) -> bool {
    started_at(live.group) == Some(live.since)
}

/// When a process started, in clock ticks since boot.
///
/// `/proc/<pid>/stat` holds the name of the executable in brackets and it may
/// contain spaces or brackets of its own, so the fields are counted from the
/// last bracket rather than from the beginning of the line. Start time is the
/// twenty-second field, which is the nineteenth after the state.
fn started_at(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(')')?.1;
    after_name.split_whitespace().nth(19)?.parse().ok()
}

fn kept() -> Vec<Live> {
    let Ok(text) = fs::read_to_string(state_file()) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save(live: &[Live]) {
    let path = state_file();
    if live.is_empty() {
        let _ = fs::remove_file(&path);
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(live) {
        let _ = fs::write(&path, text);
    }
}

fn state_file() -> PathBuf {
    crate::config::config_dir().join("running.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test's own process, which is certainly running and certainly
    /// started at some point.
    #[test]
    fn a_process_that_is_running_is_recognised() {
        let mine = std::process::id() as i32;
        let since = started_at(mine).expect("this process has a start time");
        assert!(since > 0);
        assert!(is_alive(&Live {
            app_name: "fowl".into(),
            group: mine,
            since,
        }));

        // The same pid with a different start time is a different process, and
        // that is the case the start time is here to catch.
        assert!(!is_alive(&Live {
            app_name: "fowl".into(),
            group: mine,
            since: since + 1,
        }));
    }

    /// Pid 0 is the kernel's own and never appears in /proc, which stands in
    /// here for a process that has gone.
    #[test]
    fn a_process_that_is_gone_is_not_mistaken_for_one_that_is_not() {
        assert_eq!(started_at(0), None);
        assert!(!is_alive(&Live {
            app_name: "fowl".into(),
            group: 0,
            since: 1,
        }));
    }
}
