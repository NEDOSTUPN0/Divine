//! The one HTTP client for everything that is not Epic.
//!
//! Divine asks a handful of unrelated services about a game — GOG's database
//! for a Steam id, umu's for a protonfix, ProtonDB for a rating — and downloads
//! runtimes from a couple more. None of them care who is asking, so unlike
//! [`crate::epic`], which has to introduce itself as the launcher, everything
//! here says it is Divine and differs only in how long it is willing to wait.
//!
//! The lookups also share a shape worth naming once. Each is a question a
//! service may simply not have an answer to — a game it has never heard of is
//! a 404, which is an ordinary outcome rather than a failure — and each is
//! asked from the UI thread while cards scroll past, so the answer is worth
//! remembering. [`ask`] is the first half of that and [`remembered`] the other.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::log::debug;

pub const USER_AGENT: &str = concat!("divine/", env!("CARGO_PKG_VERSION"));

/// How long a lookup may take before it is not worth the wait. Short, because
/// nothing depends on the answer: a rating that never arrives is a badge that
/// never appears.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// An agent that gives up after `timeout`. Downloads pick their own, since a
/// runtime tarball is allowed to take rather longer than a question.
pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(USER_AGENT)
        .proxy(proxy())
        .build()
        .new_agent()
}

/// The relay while a proxy is set in the preferences, and otherwise whatever
/// the environment names, which is what an agent does left to itself.
pub fn proxy() -> Option<ureq::Proxy> {
    match crate::relay::address() {
        Some(relay) => ureq::Proxy::new(&relay).ok(),
        None => ureq::Proxy::try_from_env(),
    }
}

/// Asks a service a question and hands back the body, or `None` when there was
/// no answer to be had — the service does not know this game, or is down, or
/// took too long. All three mean the same thing here: show what we have.
pub fn ask(url: &str) -> Option<String> {
    ask_with(url, &[])
}

/// [`ask`], for the services that take their question as query parameters.
pub fn ask_with(url: &str, query: &[(&str, &str)]) -> Option<String> {
    debug!("asking {url}");
    let mut request = agent(LOOKUP_TIMEOUT).get(url);
    for (key, value) in query {
        request = request.query(*key, *value);
    }
    request.call().ok()?.into_body().read_to_string().ok()
}

/// What a lookup has answered so far this session.
pub type Memo<K, V> = OnceLock<Mutex<HashMap<K, V>>>;

/// Answers from the memo when it can, and asks `look_up` when it cannot.
///
/// Misses are remembered along with hits. A game a database has never heard of
/// will not appear halfway through a session, and the library asks once per
/// card drawn, so forgetting the misses would mean a request every time the
/// list scrolls.
pub fn remembered<K, V>(memo: &Memo<K, V>, key: K, look_up: impl FnOnce(&K) -> V) -> V
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    let memo = memo.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(seen) = memo.lock() {
        if let Some(answer) = seen.get(&key) {
            return answer.clone();
        }
    }

    // Deliberately not holding the lock across the request: it would turn a
    // slow service into a frozen library, and asking twice costs nothing.
    let answer = look_up(&key);
    if let Ok(mut seen) = memo.lock() {
        seen.insert(key, answer.clone());
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_only_looked_up_once() {
        static MEMO: Memo<&str, u32> = OnceLock::new();
        let mut asked = 0;
        let mut count = |_: &&str| {
            asked += 1;
            7
        };
        assert_eq!(remembered(&MEMO, "fowl", &mut count), 7);
        assert_eq!(remembered(&MEMO, "fowl", &mut count), 7);
        assert_eq!(asked, 1);
    }

    /// The point of the memo: a service that said "never heard of it" is not
    /// asked again either.
    #[test]
    fn a_miss_is_remembered_like_any_other_answer() {
        static MEMO: Memo<&str, Option<u32>> = OnceLock::new();
        let mut asked = 0;
        let mut miss = |_: &&str| {
            asked += 1;
            None
        };
        assert_eq!(remembered(&MEMO, "peony", &mut miss), None);
        assert_eq!(remembered(&MEMO, "peony", &mut miss), None);
        assert_eq!(asked, 1);
    }
}
