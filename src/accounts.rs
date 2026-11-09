//! Several Epic accounts on one machine.
//!
//! legendary knows one account at a time: whichever `user.json` sits in its
//! config directory. Divine keeps each other account's session in a slot of its
//! own and swaps it in on demand. Everything else legendary keeps — installed
//! games, manifests, config.ini — stays shared, so a game owned by two accounts
//! is installed once, as with Epic's own launcher.
//!
//! The live files are the truth for the active account. Its slot is only
//! brought up to date on the way out, because legendary rotates the refresh
//! token every time it renews the session and a copy taken earlier would be one
//! that Epic no longer accepts.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::legendary::Legendary;

/// What legendary keeps per account. The session itself, plus the library it
/// last fetched for it: Divine's offline view reads the latter, and leaving one
/// account's copy behind for the next would show the wrong games.
const SESSION_FILES: [&str; 3] = ["user.json", "assets.json", "entitlements.json"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
}

/// The account a session file belongs to, or None when there is no usable one.
pub fn read(user_file: &Path) -> Option<Account> {
    #[derive(Deserialize)]
    struct Raw {
        account_id: String,
        #[serde(rename = "displayName")]
        display_name: String,
    }
    let text = fs::read_to_string(user_file).ok()?;
    let raw: Raw = serde_json::from_str(&text).ok()?;
    // The id becomes a directory name, so anything Epic would never issue is
    // refused rather than allowed to name a path.
    if raw.account_id.is_empty() || !raw.account_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(Account {
        id: raw.account_id,
        name: raw.display_name,
    })
}

/// Every account Divine holds a session for, the active one included, sorted
/// by name.
pub fn all(live: &Path, slots: &Path) -> Vec<Account> {
    let mut accounts: Vec<Account> = read(&live.join("user.json")).into_iter().collect();
    for slot in fs::read_dir(slots).into_iter().flatten().flatten() {
        if let Some(account) = read(&slot.path().join("user.json")) {
            if !accounts.iter().any(|known| known.id == account.id) {
                accounts.push(account);
            }
        }
    }
    accounts.sort_by_key(|account| account.name.to_lowercase());
    accounts
}

/// Signs in to an account, adding it to the ones Divine holds, and makes it
/// the active one.
///
/// legendary refuses a new sign-in while the session it has is still good, and
/// would overwrite it if it did not, so the code is redeemed in a directory of
/// its own and only the session that comes out of it is carried over.
pub fn sign_in(lgd: &Legendary, code: &str) -> Result<Account> {
    let staging = crate::config::config_dir().join("signing-in");
    let result = sign_in_through(lgd, &staging, code);
    if let Err(e) = fs::remove_dir_all(&staging) {
        if e.kind() != io::ErrorKind::NotFound {
            crate::log::warn!("cannot clear {}: {e:#}", staging.display());
        }
    }
    result
}

fn sign_in_through(lgd: &Legendary, staging: &Path, code: &str) -> Result<Account> {
    // Left over from a sign-in that never finished; Divine made it.
    match fs::remove_dir_all(staging) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("clearing {}", staging.display()));
        }
        _ => {}
    }
    private_dir(staging)?;
    lgd.in_dir(staging).auth(code)?;
    adopt(
        lgd.config_dir(),
        &crate::config::accounts_dir(),
        &staging.join("user.json"),
    )
}

/// Takes in a session legendary wrote somewhere else and makes it the active
/// one. Signing in again to an account Divine already holds replaces what it
/// kept with the fresh session.
fn adopt(live: &Path, slots: &Path, session: &Path) -> Result<Account> {
    let account = read(session).context("legendary wrote no usable session")?;
    let _held = lock(live)?;
    // Before the new session lands, or signing in again to the active account
    // would have the old session stashed over the new one.
    stash(live, slots)?;
    let slot = slot_dir(slots, &account.id)?;
    write_private(session, &slot.join("user.json"))?;
    restore(live, slots, &account.id)
}

/// Signs the active account out and brings in another, if there is one left.
pub fn sign_out(lgd: &Legendary) -> Result<Option<Account>> {
    let slots = crate::config::accounts_dir();
    let leaving = lgd.account();
    lgd.logout()?;
    if let Some(leaving) = leaving {
        forget(&slots, &leaving.id)?;
    }
    match all(lgd.config_dir(), &slots).first() {
        Some(next) => switch(lgd.config_dir(), &slots, &next.id).map(Some),
        None => Ok(None),
    }
}

/// Makes another account the one legendary sees, keeping the outgoing one's
/// session in its slot.
pub fn switch(live: &Path, slots: &Path, id: &str) -> Result<Account> {
    let _held = lock(live)?;
    stash(live, slots)?;
    restore(live, slots, id)
}

/// Copies an account's slot over the live files. The caller holds the lock.
fn restore(live: &Path, slots: &Path, id: &str) -> Result<Account> {
    let from = slot_dir(slots, id)?;
    let account = read(&from.join("user.json"))
        .with_context(|| format!("no session is kept for account {id}"))?;
    for name in SESSION_FILES {
        replace(&from.join(name), &live.join(name))?;
    }
    Ok(account)
}

/// Copies the active account's session into its slot.
fn stash(live: &Path, slots: &Path) -> Result<()> {
    let Some(active) = read(&live.join("user.json")) else {
        return Ok(());
    };
    let slot = slot_dir(slots, &active.id)?;
    for name in SESSION_FILES {
        replace(&live.join(name), &slot.join(name))?;
    }
    Ok(())
}

/// Drops the session Divine kept for an account.
fn forget(slots: &Path, id: &str) -> Result<()> {
    let slot = slots.join(id);
    match fs::remove_dir_all(&slot) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", slot.display()))
        }
        _ => Ok(()),
    }
}

/// Every slot directory, for anything that has to know what the accounts
/// Divine is not showing own.
pub fn slots(slots: &Path) -> Vec<PathBuf> {
    fs::read_dir(slots)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn slot_dir(slots: &Path, id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("{id:?} is not an Epic account id");
    }
    private_dir(slots)?;
    let slot = slots.join(id);
    private_dir(&slot)?;
    Ok(slot)
}

/// The directory holds refresh tokens, which are good for a year.
fn private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Makes `to` a copy of `from`, or removes it when there is no `from`.
fn replace(from: &Path, to: &Path) -> Result<()> {
    if from.exists() {
        return write_private(from, to);
    }
    match fs::remove_file(to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", to.display()))
        }
        _ => Ok(()),
    }
}

/// Copies through a temporary file, so a crash halfway leaves the old session
/// rather than half of the new one.
fn write_private(from: &Path, to: &Path) -> Result<()> {
    let bytes = fs::read(from).with_context(|| format!("reading {}", from.display()))?;
    let partial = to.with_extension("partial");
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| io::Write::write_all(&mut file, &bytes))
        .with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, to).with_context(|| format!("replacing {}", to.display()))
}

/// The lock legendary itself takes around reading, renewing and writing the
/// session (filelock's flock on `user.json.lock`). Holding it means a legendary
/// still busy with the old account cannot write its session on top of the new
/// one. Released when the file is closed.
fn lock(live: &Path) -> Result<fs::File> {
    let path = live.join("user.json.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: flock on a descriptor this function owns for as long as the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("locking {}", path.display()));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("divine-accounts-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("live")).unwrap();
            Scratch(dir)
        }
        fn live(&self) -> PathBuf {
            self.0.join("live")
        }
        fn slots(&self) -> PathBuf {
            self.0.join("accounts")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The fields legendary writes that this module reads, from a real
    /// user.json with the tokens cut short.
    fn session(id: &str, name: &str, refresh: &str) -> String {
        format!(
            r#"{{"access_token":"eg1~a","account_id":"{id}","displayName":"{name}",
                "expires_at":"2026-11-10T09:31:17.446Z","refresh_token":"{refresh}",
                "refresh_expires_at":"2027-11-08T22:37:57.448Z","token_type":"bearer"}}"#
        )
    }

    /// What `sign_in` does once legendary has written the session.
    fn sign_in(scratch: &Scratch, id: &str, name: &str) {
        sign_in_with(scratch, id, name, "first");
    }

    fn sign_in_with(scratch: &Scratch, id: &str, name: &str, refresh: &str) {
        let staged = scratch.0.join("staged.json");
        fs::write(&staged, session(id, name, refresh)).unwrap();
        adopt(&scratch.live(), &scratch.slots(), &staged).unwrap();
    }

    #[test]
    fn signing_in_again_keeps_the_fresh_session() {
        let scratch = Scratch::new("again");
        sign_in(&scratch, "aaaa1111", "Alice");
        sign_in_with(&scratch, "aaaa1111", "Alice", "fresh");
        let live = fs::read_to_string(scratch.live().join("user.json")).unwrap();
        assert!(live.contains("fresh"));
    }

    #[test]
    fn switching_back_brings_the_renewed_session_not_the_one_first_saved() {
        let scratch = Scratch::new("renewed");
        sign_in(&scratch, "aaaa1111", "Alice");
        // legendary renews the token while Alice plays.
        fs::write(
            scratch.live().join("user.json"),
            session("aaaa1111", "Alice", "renewed"),
        )
        .unwrap();

        sign_in(&scratch, "bbbb2222", "Bob");
        switch(&scratch.live(), &scratch.slots(), "aaaa1111").unwrap();

        let live = fs::read_to_string(scratch.live().join("user.json")).unwrap();
        assert!(live.contains("renewed"));
    }

    #[test]
    fn one_accounts_library_does_not_stay_behind_for_the_next() {
        let scratch = Scratch::new("library");
        sign_in(&scratch, "aaaa1111", "Alice");
        fs::write(scratch.live().join("assets.json"), "alice's games").unwrap();

        sign_in(&scratch, "bbbb2222", "Bob");
        assert!(!scratch.live().join("assets.json").exists());

        switch(&scratch.live(), &scratch.slots(), "aaaa1111").unwrap();
        assert_eq!(
            fs::read_to_string(scratch.live().join("assets.json")).unwrap(),
            "alice's games"
        );
    }

    #[test]
    fn what_both_accounts_share_is_left_alone_by_a_switch() {
        let scratch = Scratch::new("shared");
        fs::write(scratch.live().join("installed.json"), "{}").unwrap();
        sign_in(&scratch, "aaaa1111", "Alice");
        sign_in(&scratch, "bbbb2222", "Bob");
        assert_eq!(
            fs::read_to_string(scratch.live().join("installed.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn the_account_already_signed_in_is_listed_without_a_slot() {
        let scratch = Scratch::new("existing");
        fs::write(
            scratch.live().join("user.json"),
            session("aaaa1111", "Alice", "first"),
        )
        .unwrap();
        let slot = slot_dir(&scratch.slots(), "bbbb2222").unwrap();
        fs::write(slot.join("user.json"), session("bbbb2222", "bob", "first")).unwrap();
        // Bob's slot is all there is of him; Alice is live only, the way an
        // account signed in before Divine knew more than one is found.

        let names: Vec<String> = all(&scratch.live(), &scratch.slots())
            .into_iter()
            .map(|account| account.name)
            .collect();
        assert_eq!(names, ["Alice", "bob"]);
    }

    #[test]
    fn an_account_is_listed_once_while_it_is_active() {
        let scratch = Scratch::new("once");
        sign_in(&scratch, "aaaa1111", "Alice");
        sign_in(&scratch, "bbbb2222", "Bob");
        switch(&scratch.live(), &scratch.slots(), "aaaa1111").unwrap();
        assert_eq!(all(&scratch.live(), &scratch.slots()).len(), 2);
    }

    #[test]
    fn a_session_is_readable_by_its_owner_alone() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new("private");
        sign_in(&scratch, "aaaa1111", "Alice");
        sign_in(&scratch, "bbbb2222", "Bob");
        let kept = scratch.slots().join("aaaa1111").join("user.json");
        assert_eq!(
            fs::metadata(kept).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_forgotten_account_is_gone_from_the_list() {
        let scratch = Scratch::new("forget");
        sign_in(&scratch, "aaaa1111", "Alice");
        sign_in(&scratch, "bbbb2222", "Bob");
        forget(&scratch.slots(), "aaaa1111").unwrap();
        let ids: Vec<String> = all(&scratch.live(), &scratch.slots())
            .into_iter()
            .map(|account| account.id)
            .collect();
        assert_eq!(ids, ["bbbb2222"]);
    }

    #[test]
    fn an_id_that_could_name_a_path_is_refused() {
        let scratch = Scratch::new("path");
        assert!(slot_dir(&scratch.slots(), "../live").is_err());
        fs::write(
            scratch.live().join("user.json"),
            session("../../etc", "Mallory", "first"),
        )
        .unwrap();
        assert!(read(&scratch.live().join("user.json")).is_none());
    }
}
