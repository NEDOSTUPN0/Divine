//! Play sessions, kept in step with Epic's own counter.
//!
//! Epic's launcher reports one record per session, sent when the game exits,
//! to a service that is not documented anywhere public. Everything below was
//! read off the wire of launcher 20.2.1 rather than guessed, so Divine sends
//! exactly what Windows sends. See `docs/epic-playtime.md` for the capture.
//!
//! Two properties of the service shape the design. The record carries the
//! start and end of the whole session, not a delta, and the server keys on
//! `(machineId, artifactId, startTime)` keeping the longest span it has seen —
//! so re-sending a session can never inflate the total, and a queue of unsent
//! sessions may retry as bluntly as it likes. The launcher also journals the
//! session it is playing to disk every few minutes, so that losing the process
//! costs a few minutes rather than the evening; Divine keeps the same journal.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{launcher_agent, User};

use crate::log::warn;

const LIBRARY: &str = "https://library-service.live.use1a.on.epicgames.com/library/api/public";
const TIMEOUT: Duration = Duration::from_secs(20);
/// How often a session in progress is written down. The launcher uses roughly
/// this, and it is the most time a hard kill can cost.
pub const CHECKPOINT: Duration = Duration::from_secs(180);
/// How old a session may be and still be worth reporting. Reporting can be
/// turned on long after Divine started keeping the journal, and handing Epic
/// months of history at once would look like anything but playing games.
/// Older sessions stay on this machine as local history.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// One play session, in the shape it goes on the wire.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub app_name: String,
    /// ISO-8601 UTC with milliseconds, exactly as Epic wants it.
    pub start: String,
    pub end: String,
    /// False while the game is still running, or when Divine died with it.
    #[serde(default)]
    pub done: bool,
}

impl Session {
    pub fn started(app_name: &str) -> Self {
        let now = timestamp();
        Session {
            app_name: app_name.to_string(),
            start: now.clone(),
            end: now,
            done: false,
        }
    }

    /// Seconds between start and end, for showing time Epic has not counted yet.
    pub fn seconds(&self) -> u64 {
        let (Some(start), Some(end)) = (parse(&self.start), parse(&self.end)) else {
            return 0;
        };
        end.saturating_sub(start) / 1000
    }
}

/// Per-game totals as Epic has them, in seconds. The keys are app names:
/// Epic's `artifactId` is what legendary calls `app_name`.
pub fn totals(user_file: &Path) -> Result<HashMap<String, u64>> {
    #[derive(Deserialize)]
    struct Entry {
        #[serde(rename = "artifactId")]
        artifact_id: String,
        #[serde(rename = "totalTime")]
        total_time: u64,
    }

    let user = User::read(user_file)?;
    let body = launcher_agent(TIMEOUT)
        .get(format!(
            "{LIBRARY}/playtime/account/{}/all",
            user.account_id
        ))
        .header("Authorization", &user.authorization())
        .call()
        .context("asking Epic for playtime")?
        .into_body()
        .read_to_string()
        .context("reading the playtime response")?;

    let entries: Vec<Entry> = serde_json::from_str(&body).context("parsing playtime")?;
    Ok(entries
        .into_iter()
        .map(|entry| (entry.artifact_id, entry.total_time))
        .collect())
}

/// Reports sessions to Epic. Sending the same session twice is harmless, so a
/// caller that is unsure whether the last attempt landed should just send again.
pub fn report(user_file: &Path, sessions: &[Session]) -> Result<()> {
    if sessions.is_empty() {
        return Ok(());
    }
    let user = User::read(user_file)?;
    let machine = machine_id()?;
    let records: Vec<serde_json::Value> = sessions
        .iter()
        .map(|session| {
            serde_json::json!({
                "machineId": machine,
                "artifactId": session.app_name,
                "startTime": session.start,
                "endTime": session.end,
                "startSegment": true,
                // A session Divine did not see end is reported as unfinished,
                // which is what the launcher does with a journal it recovers.
                "endSegment": session.done,
            })
        })
        .collect();

    let body = serde_json::to_string(&serde_json::json!({ "playtimeAddList": records }))
        .context("building the playtime report")?;
    let response = launcher_agent(TIMEOUT)
        .put(format!(
            "{LIBRARY}/playtime/account/{}/bulk",
            user.account_id
        ))
        .header("Authorization", &user.authorization())
        .header("Content-Type", "application/json")
        .send(&body)
        .context("reporting playtime to Epic")?;
    if response.status() != 204 {
        bail!("Epic answered {} to the playtime report", response.status());
    }
    Ok(())
}

/// Keeps a session in the journal for as long as a game runs, refreshing it
/// every [`CHECKPOINT`] so that losing Divine costs those minutes rather than
/// the whole evening. Epic's launcher keeps the same journal for the same
/// reason, and recovers it the same way: on the next report.
pub struct Watch {
    session: Arc<Mutex<Session>>,
    stop: mpsc::Sender<()>,
    clock: thread::JoinHandle<()>,
}

impl Watch {
    pub fn start(app_name: &str) -> Self {
        let session = Arc::new(Mutex::new(Session::started(app_name)));
        if let Ok(session) = session.lock() {
            remember(&session);
        }

        let (stop, halt) = mpsc::channel();
        let ticking = Arc::clone(&session);
        let clock = thread::spawn(move || {
            // Any answer other than "nothing yet" means the game is over.
            while matches!(
                halt.recv_timeout(CHECKPOINT),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                if let Ok(mut session) = ticking.lock() {
                    session.end = timestamp();
                    remember(&session);
                }
            }
        });

        Watch {
            session,
            stop,
            clock,
        }
    }

    /// How long the game has been up, for deciding whether it ran at all.
    pub fn seconds(&self) -> u64 {
        self.session
            .lock()
            .map(|session| {
                parse(&session.start)
                    .and_then(|start| parse(&timestamp()).map(|now| (now - start) / 1000))
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// Closes the session and, when reporting is on, empties the journal.
    pub fn finish(self, user_file: &Path, to_epic: bool) {
        let session = self.halt();
        if let Ok(mut session) = session.lock() {
            session.end = timestamp();
            session.done = true;
            remember(&session);
        }
        if to_epic {
            if let Err(e) = flush(user_file) {
                // Nothing is lost: the journal keeps it for the next attempt.
                warn!("this session is not with Epic yet: {e:#}");
            }
        }
    }

    /// Throws the session away, for a launch that never became a game.
    pub fn discard(self) {
        let session = self.halt();
        if let Ok(session) = session.lock() {
            forget(&session);
        };
    }

    fn halt(self) -> Arc<Mutex<Session>> {
        let _ = self.stop.send(());
        let _ = self.clock.join();
        self.session
    }
}

/// The session still open for a game, if the journal holds one.
///
/// A session is only left open when Divine stopped before the game did. The
/// newest one wins: an older open session belongs to a run nobody closed and
/// is not the one still going.
fn open_session(app_name: &str) -> Option<Session> {
    pending()
        .into_iter()
        .filter(|session| session.app_name == app_name && !session.done)
        .max_by(|one, other| one.start.cmp(&other.start))
}

/// Moves the end of an open session along, for a game Divine is watching from
/// the outside rather than running itself. Same purpose as [`Watch`]'s own
/// ticking: what a hard kill costs is the time since the last one.
pub fn checkpoint(app_name: &str) {
    if let Some(mut session) = open_session(app_name) {
        session.end = timestamp();
        remember(&session);
    }
}

/// Closes a session Divine did not see end, because the game outlived the
/// launcher. The journal already holds the real start, so only the end is new.
pub fn close(app_name: &str, user_file: &Path, to_epic: bool) {
    let Some(mut session) = open_session(app_name) else {
        return;
    };
    session.end = timestamp();
    session.done = true;
    remember(&session);

    if to_epic {
        if let Err(e) = flush(user_file) {
            // Nothing is lost: the journal keeps it for the next attempt.
            warn!("this session is not with Epic yet: {e:#}");
        }
    }
}

/// Splits the journal into what may still go to Epic and what has aged out of
/// reporting. Both halves keep counting towards what Divine shows.
fn by_age(sessions: Vec<Session>) -> (Vec<Session>, Vec<Session>) {
    let cutoff = parse(&timestamp())
        .unwrap_or_default()
        .saturating_sub(MAX_AGE.as_millis() as u64);
    sessions
        .into_iter()
        .partition(|session| parse(&session.end).is_some_and(|end| end >= cutoff))
}

/// Sessions too old to report, which Epic will therefore never count.
pub fn stale() -> Vec<Session> {
    by_age(pending()).1
}

/// Hands the journal to Epic and drops what it accepted. Everything goes in
/// one request however many games it covers, which is how the launcher empties
/// its own cache, and is why a backlog cannot turn into a burst of traffic.
pub fn flush(user_file: &Path) -> Result<()> {
    let (fresh, _) = by_age(pending());
    report(user_file, &fresh)?;
    for session in &fresh {
        forget(session);
    }
    Ok(())
}

/// The sessions still waiting to reach Epic, oldest first.
pub fn pending() -> Vec<Session> {
    let Ok(text) = fs::read_to_string(journal_file()) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Replaces the journal. Writing an empty list clears it.
pub fn save(sessions: &[Session]) {
    let path = journal_file();
    if sessions.is_empty() {
        let _ = fs::remove_file(&path);
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(sessions) {
        let _ = fs::write(&path, text);
    }
}

/// Writes a session down, replacing an earlier record of the same run. The
/// journal is keyed by app name and start, which is Epic's own key.
pub fn remember(session: &Session) {
    let mut sessions = pending();
    match sessions
        .iter_mut()
        .find(|old| old.app_name == session.app_name && old.start == session.start)
    {
        Some(old) => *old = session.clone(),
        None => sessions.push(session.clone()),
    }
    save(&sessions);
}

/// Drops one session from the journal, after Epic has taken it.
pub fn forget(session: &Session) {
    let sessions: Vec<Session> = pending()
        .into_iter()
        .filter(|old| !(old.app_name == session.app_name && old.start == session.start))
        .collect();
    save(&sessions);
}

/// Time Divine has recorded but Epic has not yet counted, per game.
pub fn unreported() -> HashMap<String, u64> {
    let mut extra: HashMap<String, u64> = HashMap::new();
    for session in pending() {
        *extra.entry(session.app_name.clone()).or_default() += session.seconds();
    }
    extra
}

/// "12 h 34 min", the way a launcher writes it. Under a minute is not a
/// session worth naming, so it reads as none.
pub fn human(seconds: u64) -> String {
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "None".to_string(),
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

fn journal_file() -> PathBuf {
    crate::config::config_dir().join("playtime.json")
}

/// A UUIDv4 identifying this installation, as the launcher sends per machine.
/// Generated once and kept, because Epic keys deduplication on it.
fn machine_id() -> Result<String> {
    let path = crate::config::config_dir().join("machine-id");
    if let Ok(existing) = fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let id = random_uuid()?;
    fs::write(&path, &id).context("saving the machine id")?;
    Ok(id)
}

fn random_uuid() -> Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .context("opening /dev/urandom")?
        .read_exact(&mut bytes)
        .context("reading random bytes")?;
    // Version 4, variant 1, as every other UUIDv4 on the wire.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// Now, as Epic writes it: "2026-08-19T00:42:04.532Z".
pub fn timestamp() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default();
    format(millis)
}

fn format(millis: u64) -> String {
    let seconds = millis / 1000;
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60,
        millis % 1000
    )
}

fn parse(stamp: &str) -> Option<u64> {
    let (date, time) = stamp.split_once('T')?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;

    let time = time.trim_end_matches('Z');
    let (clock, millis) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock = clock.split(':');
    let hours: u64 = clock.next()?.parse().ok()?;
    let minutes: u64 = clock.next()?.parse().ok()?;
    let seconds: u64 = clock.next()?.parse().ok()?;

    let days = days_from_civil(year, month, day);
    let total = days.checked_mul(86_400)? + (hours * 3600 + minutes * 60 + seconds) as i64;
    let millis: u64 = millis.parse().ok()?;
    u64::try_from(total)
        .ok()?
        .checked_mul(1000)?
        .checked_add(millis)
}

/// Days since 1970-01-01 to a civil date, and back. Both are Howard Hinnant's
/// algorithms, which are exact for every date this will ever see.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_match_what_epic_sends() {
        // The exact value captured from launcher 20.2.1.
        assert_eq!(format(1_787_100_124_532), "2026-08-19T00:42:04.532Z");
        assert_eq!(format(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_timestamp_survives_a_round_trip() {
        for millis in [0, 1_000, 1_787_100_124_532, 4_102_444_800_000] {
            assert_eq!(parse(&format(millis)), Some(millis), "at {millis}");
        }
    }

    #[test]
    fn a_session_knows_how_long_it_ran() {
        let session = Session {
            app_name: "Fowl".into(),
            start: "2026-08-19T00:54:20.173Z".into(),
            end: "2026-08-19T01:05:23.602Z".into(),
            done: true,
        };
        // The 11 minute session from the capture.
        assert_eq!(session.seconds(), 663);
    }

    #[test]
    fn a_length_reads_like_a_launcher() {
        assert_eq!(human(0), "None");
        assert_eq!(human(59), "None");
        assert_eq!(human(60), "1 min");
        assert_eq!(human(3_600), "1 h");
        assert_eq!(human(3_660), "1 h 1 min");
    }

    #[test]
    fn only_the_last_day_is_worth_reporting() {
        let now = parse(&timestamp()).unwrap();
        let at = |ago: u64| format(now - ago * 1000);
        let session = |ago: u64| Session {
            app_name: "Fowl".into(),
            start: at(ago + 600),
            end: at(ago),
            done: true,
        };

        let (fresh, stale) = by_age(vec![session(60), session(25 * 3600), session(3600)]);
        assert_eq!(fresh.len(), 2);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].end, at(25 * 3600));
    }

    #[test]
    fn a_machine_id_looks_like_a_uuid4() {
        let id = random_uuid().unwrap();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(id.is_char_boundary(0) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert_eq!(parts[2].as_bytes()[0], b'4');
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
    }
}
