//! The library collections Epic's own launcher shows down its left side, and
//! the star it calls Favorites.
//!
//! legendary has no command for these, so Divine talks to the library service
//! directly — the same one the launcher uses, and the same one the store's
//! GraphQL gateway forwards to. Divine went through that gateway at first; on
//! 2026-08-20 every collection call through it began answering
//! "Authentication failed for /api/public/collection/… (status: 401)" while the
//! very same token still worked against the service itself. The gateway is a
//! shopfront holding a launcher's token; this is the launcher's own door, and
//! it is the shorter way in regardless.
//!
//! Three things about the service are load-bearing.
//!
//! Favorites is not special. It is an ordinary collection under a fixed id, so
//! one set of calls covers everything — except that Epic will not let it be
//! renamed or deleted, and neither will Divine. See [`Collection::editable`].
//!
//! Membership is keyed by the sandbox, which is the game's namespace. Records
//! carry a catalog id beside it, but that names the catalogue entry rather than
//! the game, and a game can have several: the namespace is what the library is
//! matched on.
//!
//! Putting a game in a collection needs both ids, one after the other in the
//! path, which is why [`crate::models::Game`] carries a catalog id.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::User;

/// The library service, as the launcher addresses it.
const SERVICE: &str = "https://library-service.live.use1a.on.epicgames.com/library/api/public";

/// Long enough for a slow answer, short enough that a sidebar waiting on it
/// does not look broken.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The id Epic gives the star. Fixed, and the same on every account.
const FAVORITES: &str = "EGS_STATIC_FAV_COLLECTION";

/// Epic rejects a longer name with a 400, so the entry has to stop here rather
/// than let the user type something that cannot be saved.
pub const MAX_NAME: usize = 25;

/// Including Favorites. The service refuses the next one with
/// "maximum library collections have been created for account".
pub const MAX_COLLECTIONS: usize = 12;

/// One collection and everything in it.
#[derive(Debug, Clone)]
pub struct Collection {
    pub id: String,
    pub name: String,
    /// Namespaces of the games in it, which is what the library is matched on.
    pub members: Vec<String>,
}

impl Collection {
    pub fn is_favorites(&self) -> bool {
        self.id == FAVORITES
    }

    /// Whether it can be renamed or deleted. Only Favorites cannot.
    pub fn editable(&self) -> bool {
        !self.is_favorites()
    }

    pub fn holds(&self, namespace: &str) -> bool {
        !namespace.is_empty() && self.members.iter().any(|held| held == namespace)
    }
}

/// What a collection needs to know about a game to hold it. Epic wants both
/// ids, one after the other in the path.
#[derive(Debug, Clone)]
pub struct Member {
    pub namespace: String,
    pub catalog_id: String,
}

/// Every collection on the account, members included.
///
/// One request for the names and one per collection for its contents: the
/// service has no way to ask for several at once, and an account is capped at
/// twelve, so this is a dozen small requests at worst.
pub fn all(user_file: &Path) -> Result<Vec<Collection>> {
    let user = User::read(user_file)?;
    let named: Named = get(&user, &format!("collection/account/{}", user.account_id))
        .context("reading the collections")?;

    named
        .collections
        .into_iter()
        .map(|raw| {
            let held: Held = get(
                &user,
                &format!("collection/{}/item/account/{}", raw.id, user.account_id),
            )
            .with_context(|| format!("reading what {} holds", raw.name))?;

            Ok(Collection {
                id: raw.id,
                name: raw.name,
                members: held.records.into_iter().map(|item| item.sandbox).collect(),
            })
        })
        .collect()
}

/// Makes a collection and hands back its id.
pub fn create(user_file: &Path, name: &str) -> Result<String> {
    let name = check_name(name)?;
    let user = User::read(user_file)?;

    let text = super::launcher_agent(TIMEOUT)
        .post(format!("{SERVICE}/collection/account/{}", user.account_id))
        .header("Authorization", &user.authorization())
        .header("Content-Type", "application/json")
        .send(named_body(name).as_str())
        .context("making the collection")?
        .into_body()
        .read_to_string()
        .context("reading the new collection")?;

    let made: RawCollection = serde_json::from_str(&text).context("parsing the new collection")?;
    Ok(made.id)
}

/// Renames a collection. Favorites is Epic's and keeps its name.
pub fn rename(user_file: &Path, collection: &Collection, name: &str) -> Result<()> {
    if !collection.editable() {
        bail!("Favorites is Epic's own collection and cannot be renamed");
    }
    let name = check_name(name)?;
    let user = User::read(user_file)?;

    super::launcher_agent(TIMEOUT)
        .put(format!(
            "{SERVICE}/collection/{}/account/{}",
            collection.id, user.account_id
        ))
        .header("Authorization", &user.authorization())
        .header("Content-Type", "application/json")
        .send(named_body(name).as_str())
        .context("renaming the collection")?;
    Ok(())
}

/// Deletes a collection. The games in it are not touched — a collection is a
/// label, not a folder.
pub fn remove(user_file: &Path, collection: &Collection) -> Result<()> {
    if !collection.editable() {
        bail!("Favorites is Epic's own collection and cannot be deleted");
    }
    let user = User::read(user_file)?;

    super::launcher_agent(TIMEOUT)
        .delete(format!(
            "{SERVICE}/collection/{}/account/{}",
            collection.id, user.account_id
        ))
        .header("Authorization", &user.authorization())
        .call()
        .context("deleting the collection")?;
    Ok(())
}

/// Puts a game in a collection, or takes it out. Both sides of one call: the
/// path is the same and only the method differs.
pub fn set_member(user_file: &Path, collection_id: &str, game: &Member, held: bool) -> Result<()> {
    let user = User::read(user_file)?;
    let url = format!(
        "{SERVICE}/collection/{collection_id}/item/account/{}/sandbox/{}/catalog/{}",
        user.account_id, game.namespace, game.catalog_id
    );

    // Nothing to send: both ids are already in the path. Adding is a POST and
    // taking out a DELETE, and ureq types the two apart, so each sends itself.
    let agent = super::launcher_agent(TIMEOUT);
    let authorization = user.authorization();
    let sent = match held {
        true => agent
            .post(url)
            .header("Authorization", &authorization)
            .send_empty(),
        false => agent
            .delete(url)
            .header("Authorization", &authorization)
            .call(),
    };
    sent.with_context(|| match held {
        true => "putting the game in the collection",
        false => "taking the game out of the collection",
    })?;
    Ok(())
}

/// The id of the Favorites collection among the ones fetched, if it is there.
pub fn favorites(collections: &[Collection]) -> Option<&Collection> {
    collections.iter().find(|c| c.is_favorites())
}

fn check_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a collection needs a name");
    }
    // Counted in characters, as the launcher counts them.
    if name.chars().count() > MAX_NAME {
        bail!("Epic allows at most {MAX_NAME} characters in a collection name");
    }
    Ok(name)
}

/// One GET against the service, parsed. Every read here has this shape.
fn get<T: serde::de::DeserializeOwned>(user: &User, path: &str) -> Result<T> {
    let text = super::launcher_agent(TIMEOUT)
        .get(format!("{SERVICE}/{path}"))
        .header("Authorization", &user.authorization())
        .call()
        .context("asking the library service")?
        .into_body()
        .read_to_string()
        .context("reading its answer")?;
    serde_json::from_str(&text).context("parsing its answer")
}

/// The one body the service ever takes: the name to give a collection.
fn named_body(name: &str) -> String {
    serde_json::json!({ "name": name }).to_string()
}

#[derive(Deserialize)]
struct Named {
    #[serde(default)]
    collections: Vec<RawCollection>,
}

#[derive(Deserialize)]
struct RawCollection {
    #[serde(rename = "collectionId")]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct Held {
    #[serde(default)]
    records: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    sandbox: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection(id: &str, name: &str, members: &[&str]) -> Collection {
        Collection {
            id: id.into(),
            name: name.into(),
            members: members.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Epic's own collection is the one Divine must not offer to change.
    #[test]
    fn favorites_is_the_only_one_that_cannot_be_edited() {
        let star = collection(FAVORITES, "Favorites", &[]);
        assert!(star.is_favorites());
        assert!(!star.editable());

        let mine = collection("f6f381b2-f807-4815-ab76-c4ab542a4b55", "Адекватнич", &[]);
        assert!(!mine.is_favorites());
        assert!(mine.editable());
    }

    /// A game with no namespace matches nothing. Epic gives one to every game it
    /// sells, but an entry Divine could not read must not fall into a collection
    /// just because its namespace is empty too.
    #[test]
    fn membership_is_by_namespace_and_an_empty_one_is_not_a_match() {
        let held = collection(FAVORITES, "Favorites", &["calluna", "catnip", ""]);
        assert!(held.holds("calluna"));
        assert!(!held.holds("angelonia"));
        assert!(!held.holds(""));
    }

    #[test]
    fn a_name_epic_would_reject_is_refused_before_the_request() {
        assert!(check_name("").is_err());
        assert!(check_name("   ").is_err());
        assert_eq!(check_name("  Racing  ").unwrap(), "Racing");
        assert!(check_name(&"x".repeat(MAX_NAME)).is_ok());
        assert!(check_name(&"x".repeat(MAX_NAME + 1)).is_err());
        // Counted in characters: 25 Cyrillic letters are 50 bytes and still fit.
        assert!(check_name(&"я".repeat(MAX_NAME)).is_ok());
        assert!(check_name(&"я".repeat(MAX_NAME + 1)).is_err());
    }

    #[test]
    fn favorites_is_found_among_the_rest() {
        let all = vec![
            collection("f6f381b2", "Адекватнич", &[]),
            collection(FAVORITES, "Favorites", &["calluna"]),
        ];
        assert_eq!(favorites(&all).map(|c| c.id.as_str()), Some(FAVORITES));
        assert!(favorites(&all[..1]).is_none());
    }

    /// The account's collections as the service lists them, trimmed from a live
    /// answer. Favorites arrives with the rest, under its fixed id.
    #[test]
    fn the_list_of_collections_parses() {
        let json = r#"{"collections":[
          {"collectionId":"EGS_STATIC_FAV_COLLECTION","name":"Favorites","libraryItemContentVersion":18},
          {"collectionId":"f6f381b2-f807-4815-ab76-c4ab542a4b55","name":"Сильно","libraryItemContentVersion":67}
        ]}"#;
        let named: Named = serde_json::from_str(json).expect("the list parses");
        assert_eq!(named.collections.len(), 2);
        assert_eq!(named.collections[0].id, FAVORITES);
        assert_eq!(named.collections[1].name, "Сильно");
    }

    /// What a collection holds, as the service writes it. The catalog id sits
    /// beside the sandbox and is dropped here: the library is joined on the
    /// namespace.
    #[test]
    fn what_a_collection_holds_parses() {
        let json = r#"{"responseMetadata":{},"records":[
          {"sandbox":"a93cd6111251461c953776a0c4ea4d80","catalogId":"d0083c0713b140709d92ac02b179cbf2"},
          {"sandbox":"7109c2e220544434ad355bce3cf69120","catalogId":"a14e80eeef704aa1894b1bf14547e069"}
        ]}"#;
        let held: Held = serde_json::from_str(json).expect("the members parse");
        let namespaces: Vec<&str> = held
            .records
            .iter()
            .map(|item| item.sandbox.as_str())
            .collect();
        assert_eq!(
            namespaces,
            [
                "a93cd6111251461c953776a0c4ea4d80",
                "7109c2e220544434ad355bce3cf69120"
            ]
        );

        // An empty collection answers in the same shape with no records.
        let empty: Held = serde_json::from_str(r#"{"responseMetadata":{}}"#).expect("empty parses");
        assert!(empty.records.is_empty());
    }
}
