//! Everything Divine asks Epic for itself.
//!
//! legendary covers the library, the downloads and the login, and where it does
//! Divine goes through it rather than around it. The gaps are what lives here:
//! the store never had a legendary command, playtime is not something legendary
//! reports, and achievements are newer than legendary's last release. Each of
//! those needs a request Epic will answer, so the pieces every such request
//! needs — a session, a user agent Epic recognises, a way to post GraphQL — sit
//! in this module and the endpoints sit in the submodules beside it.
//!
//! Two user agents, because Epic runs two kinds of service. The store is behind
//! Cloudflare and answers a client it does not recognise with a challenge page,
//! so requests to it come from a browser-ish string. The launcher services are
//! not public at all and expect the launcher, so requests to those come from
//! the launcher's own string, matching what [`playtime`] captured off the wire.

pub mod achievements;
pub mod collections;
pub mod playtime;
pub mod store;

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

/// The store sits behind Cloudflare and answers a client it does not know with
/// a challenge page rather than data. Epic's own launcher is known, so that is
/// what we introduce ourselves as — the same string Heroic uses.
const STORE_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                           (KHTML, like Gecko) EpicGamesLauncher";
/// The launcher's own user agent, for the launcher-only services.
const LAUNCHER_AGENT: &str =
    "UELauncher/20.2.1-56875188+++UE5+Release-Distro-5.5 Windows/10.0.19045.1.0.64bit";

/// An agent for the public store, which the store text and the achievement
/// catalogue both come from.
pub fn store_agent() -> ureq::Agent {
    agent(STORE_AGENT, Duration::from_secs(15))
}

/// An agent for the services only the launcher is supposed to talk to.
pub fn launcher_agent(timeout: Duration) -> ureq::Agent {
    agent(LAUNCHER_AGENT, timeout)
}

fn agent(user_agent: &str, timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(user_agent)
        .proxy(crate::http::proxy())
        .build()
        .new_agent()
}

/// Posts a GraphQL query and hands back whatever `T` makes of the answer.
///
/// `what` names the thing being asked for, in the middle of a sentence — it
/// becomes "asking Epic for the achievement list" and the like, so every error
/// says which request failed without each caller spelling that out three times.
pub fn graphql<T: DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    query: &str,
    variables: serde_json::Value,
    user: Option<&User>,
    what: &str,
) -> Result<T> {
    let body = serde_json::json!({ "query": query, "variables": variables }).to_string();

    let mut request = agent.post(url).header("Content-Type", "application/json");
    if let Some(user) = user {
        request = request.header("Authorization", &user.authorization());
    }

    let text = request
        .send(body.as_str())
        .with_context(|| format!("asking Epic for {what}"))?
        .into_body()
        .read_to_string()
        .with_context(|| format!("reading {what}"))?;

    serde_json::from_str(&text).with_context(|| format!("parsing {what}"))
}

/// The signed-in account, read from the session legendary keeps.
///
/// Divine never logs in itself — legendary owns that, refresh included — so
/// this only ever reads `user.json` and never writes it.
pub struct User {
    pub account_id: String,
    pub token_type: String,
    pub access_token: String,
}

impl User {
    pub fn read(user_file: &Path) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct Raw {
            account_id: String,
            access_token: String,
            #[serde(default)]
            token_type: String,
        }

        let text = fs::read_to_string(user_file).context("reading the legendary session")?;
        let raw: Raw = serde_json::from_str(&text).context("parsing the legendary session")?;
        Ok(User {
            account_id: raw.account_id,
            // Absent in sessions legendary wrote before it started saving it.
            token_type: if raw.token_type.is_empty() {
                "bearer".to_string()
            } else {
                raw.token_type
            },
            access_token: raw.access_token,
        })
    }

    fn authorization(&self) -> String {
        format!("{} {}", self.token_type, self.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, json: &str) -> std::path::PathBuf {
        let file =
            std::env::temp_dir().join(format!("divine-user-{name}-{}.json", std::process::id()));
        fs::write(&file, json).unwrap();
        file
    }

    /// legendary's user.json, trimmed to the three fields Divine reads.
    #[test]
    fn a_session_is_read_the_way_legendary_writes_it() {
        let file = session(
            "full",
            r#"{"access_token":"eyJhbGci","token_type":"bearer",
                "account_id":"3fd7","displayName":"someone"}"#,
        );
        let user = User::read(&file).unwrap();
        assert_eq!(user.account_id, "3fd7");
        assert_eq!(user.authorization(), "bearer eyJhbGci");
        fs::remove_file(&file).unwrap();
    }

    /// Sessions written before legendary started saving the type have none, and
    /// bearer is what it would have said.
    #[test]
    fn a_session_without_a_token_type_still_authorises() {
        let file = session("bare", r#"{"access_token":"eyJhbGci","account_id":"3fd7"}"#);
        assert_eq!(
            User::read(&file).unwrap().authorization(),
            "bearer eyJhbGci"
        );
        fs::remove_file(&file).unwrap();
    }

    /// Nobody signed in, which every caller has to survive rather than panic on.
    #[test]
    fn no_session_at_all_is_an_error_and_not_a_crash() {
        assert!(User::read(std::path::Path::new("/nowhere/user.json")).is_err());
        let file = session("broken", "half a file");
        assert!(User::read(&file).is_err());
        fs::remove_file(&file).unwrap();
    }
}
