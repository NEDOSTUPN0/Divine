//! What Epic's store page says about a game.
//!
//! The catalogue legendary hands us carries a `description` field, but for most
//! games it is just the title again — publishers fill in the store page, not
//! the catalogue entry. The store keeps its text behind a slug, and the slug
//! behind the game's namespace, so it takes two requests to get at it. Heroic
//! reaches it the same way (`storeManagers/legendary/games.ts`).

use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::log::debug;

const CATALOG: &str = "https://launcher.store.epicgames.com/graphql";
/// The shopfront, as opposed to the launcher's own catalogue. It answers about
/// what a shopper sees: the rating, and the pictures for games whose page never
/// went into the content service below.
const SHOPFRONT: &str = "https://store.epicgames.com/graphql";
const CONTENT: &str = "https://store-content.ak.epicgames.com/api/en-US/content/products";

/// The namespace names the game to Epic; the slug names its page to the store,
/// and only this mapping knows which is which.
const SLUG_QUERY: &str = r#"
query slug($namespace: String!) {
  Catalog {
    catalogNs(namespace: $namespace) {
      mappings(pageType: "productHome") { pageSlug }
    }
  }
}"#;

/// Ratings live on the shopfront rather than the launcher's catalogue, which is
/// why this query goes to a different host than the slug lookup above.
const RATING_QUERY: &str = r#"
query rating($namespace: String!) {
  RatingsPolls {
    getProductResult(sandboxId: $namespace, locale: "en") { averageRating }
  }
}"#;
/// The pictures for a game whose page the content service has none of.
///
/// Epic has two store back ends. The older one holds the built pages Divine
/// reads first, carousel and all; everything else keeps its gallery in the
/// sandbox configuration, which is what the store's own page falls back to.
/// The launcher's catalogue is no use here — its copy of the game carries the
/// box art and nothing else — so this has to be asked for.
///
/// `keyImages` holds videos as well as pictures, hence the inline fragment:
/// the two share a type and only one of them has a URL worth having.
const MEDIA_QUERY: &str = r#"
query media($sandboxId: String!) {
  Product {
    sandbox(sandboxId: $sandboxId) {
      configuration {
        ... on HomeConfiguration {
          configs { keyImages { ... on KeyImage { type url } } }
        }
      }
    }
  }
}"#;

/// What Epic calls a gallery picture. Two names for the same thing, and
/// neither is the box art, the thumbnail or the trailer.
const GALLERY: [&str; 2] = ["featuredMedia", "GalleryImage"];
/// A trailer among the key images, named for the strip of video the store page
/// opens with. Its URL is not a file but a QuickSilver id, which the query
/// below turns into one.
const TRAILER: &str = "heroCarouselVideo";

/// The files behind one QuickSilver id: an MP4 and two streaming manifests of
/// the same thing. The MP4 is what Divine takes — the server serves it in
/// parts, so a pipeline can play it without downloading it first.
const VIDEO_QUERY: &str = r#"
query video($videoId: String!) {
  Video {
    fetchQuickSilverVideo(videoId: $videoId) { output }
  }
}"#;

/// Store text changes about as often as the game does, which is to say rarely.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// An answer with nothing in it is kept for a day rather than a month. It
/// means the store had no page under the name we guessed at, which is worth
/// asking about again once the guess or the store may have changed — but not
/// worth asking about on every single visit, which is what leaves a page
/// rearranging itself in front of the reader every time it opens.
const EMPTY_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// How long to leave between games while filling the cache for a whole
/// library. This is idle work, and Epic has no reason to see a hundred
/// requests from one launcher at once.
const WARM_PAUSE: Duration = Duration::from_millis(300);
/// Bumped whenever this file starts reading more out of the store than before.
/// Serde fills anything new with defaults, so without a version a cache written
/// by an older build looks complete and quietly hides the new blocks.
const CACHE_VERSION: u32 = 5;
/// Folds the store's flat language list into the sections it actually means.
///
/// A `Kind:` prefix opens a section and every bare line after it belongs to
/// that section, so "Voice: English", "French", "German" is one row and not
/// three. Bare lines before any prefix are the game's languages outright.
pub fn language_sections(lines: &[String]) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let found = sections_on(line);
        if found.is_empty() {
            // No heading anywhere: the line belongs to whatever came before it.
            match sections.last_mut() {
                Some((_, listed)) => listed.push(line.to_string()),
                None => sections.push((String::new(), vec![line.to_string()])),
            }
            continue;
        }
        for (kind, listed) in found {
            sections.push((heading(kind), vec![listed.to_string()]));
        }
    }
    sections
        .into_iter()
        .map(|(kind, listed)| {
            let title = match kind.is_empty() {
                true => "Languages".to_string(),
                false => kind,
            };
            (title, listed.join(", "))
        })
        .collect()
}

/// Every section on one line, as `(kind, languages)`.
///
/// Publishers type these by hand, so one line holds anything from one section
/// to two run together: `AUDIO: English | TEXT: English, French` is Discord's,
/// and others part the two with a comma or a run of spaces instead. What holds
/// throughout is that a colon opens a section and no language contains one, so
/// the words in front of each colon name what follows it.
fn sections_on(line: &str) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    let mut kind_at = 0;
    let mut cursor = 0;

    while let Some(colon) = line[cursor..].find(':') {
        let colon = cursor + colon;
        let value_at = colon + 1;
        // The section runs to the next heading, or to the end of the line when
        // there is no other heading on it.
        let value_ends = match line[value_at..].find(':') {
            Some(next) => opens_heading(&line[value_at..value_at + next])
                .map(|at| value_at + at)
                .unwrap_or(value_at + next),
            None => line.len(),
        };
        found.push((
            line[kind_at..colon].trim(),
            line[value_at..value_ends]
                .trim()
                .trim_end_matches(['|', ','])
                .trim(),
        ));
        kind_at = value_ends;
        cursor = value_ends.max(value_at);
    }
    found
}

/// Where the next heading starts inside the run between two colons, which is
/// to say where the languages before it stop.
fn opens_heading(between: &str) -> Option<usize> {
    // The store's three ways of parting two sections, in the order they are
    // worth trusting: a language list can hold spaces but never a pipe, and it
    // holds single commas but never a double space.
    for parting in ['|', ','] {
        if let Some(at) = between.rfind(parting) {
            return Some(at + parting.len_utf8());
        }
    }
    if let Some(at) = between.rfind("  ") {
        return Some(at + 2);
    }
    // Nothing parts them, so the heading is the last word before the colon.
    between.rfind(char::is_whitespace).map(|at| at + 1)
}

/// "AUDIO" reads as shouting in a row title and the store writes it that way,
/// but anything already mixed case is left alone: some of these headings are
/// whole sentences, like "Coming 30th January 2020".
fn heading(raw: &str) -> String {
    if raw.chars().any(char::is_lowercase) {
        return raw.to_string();
    }
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// What the store page holds about a game. The descriptions come as they were
/// written: the long one is Markdown, the short one plain.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct About {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub short: String,
    #[serde(default)]
    pub long: String,
    #[serde(default)]
    pub screenshots: Vec<String>,
    /// Trailers, shown in the same carousel as the pictures.
    #[serde(default)]
    pub videos: Vec<Clip>,
    /// Genres as the store files them, already turned into words.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    /// One line each for audio and text, as the store writes them.
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub publisher: String,
    /// Epic's player rating out of five. Only the average is published — how
    /// many people voted is not — so zero means nobody has.
    #[serde(default)]
    pub rating: f64,
    /// The age rating for this part of the world, where the store has one.
    #[serde(default)]
    pub age_rating: String,
    /// What the age rating was given for, in the store's own words.
    #[serde(default)]
    pub age_descriptors: Vec<String>,
}

impl About {
    /// Nothing here for a page to show. Either the store has no page for this
    /// game or it could not be found, and either way the page is the same.
    fn is_bare(&self) -> bool {
        self.short.is_empty()
            && self.long.is_empty()
            && self.screenshots.is_empty()
            && self.videos.is_empty()
            && self.tags.is_empty()
            && self.links.is_empty()
            && self.requirements.is_empty()
            && self.languages.is_empty()
            && self.age_rating.is_empty()
            && self.rating <= 0.0
    }
}

/// One trailer: the file to play, and the frame to show until it is played.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Clip {
    pub url: String,
    pub poster: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Link {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Requirement {
    pub title: String,
    pub minimum: String,
    pub recommended: String,
}

/// The answer already on disk, if there is one and it is still current.
///
/// Worth asking before building a page: it is one small file, and a page that
/// can be drawn complete in one go never rearranges itself under the reader.
pub fn cached(namespace: &str) -> Option<About> {
    let path = cache_path(namespace);
    let about: About = serde_json::from_str(&fs::read_to_string(&path).ok()?).ok()?;
    if about.version != CACHE_VERSION {
        return None;
    }
    let keep = match about.is_bare() {
        true => EMPTY_MAX_AGE,
        false => MAX_AGE,
    };
    (!older_than(&path, keep)).then_some(about)
}

fn cache_path(namespace: &str) -> std::path::PathBuf {
    crate::config::store_dir().join(format!("{namespace}.json"))
}

/// The store text for a game, cached on disk. Needs the network the first time.
pub fn about(namespace: &str, title: &str) -> Result<About> {
    if namespace.is_empty() {
        return Ok(About::default());
    }
    if let Some(found) = cached(namespace) {
        return Ok(found);
    }
    let cached = cache_path(namespace);

    // A game pulled from the store keeps its slug guess; the request simply
    // fails and the catalogue text stands in.
    let slug = product_slug(namespace).unwrap_or_else(|| slug_from_title(title));
    let mut about = match fetch(&slug) {
        Ok(about) => about,
        // The store has never heard of that name. That is an answer, and one
        // worth writing down: without it the page has to ask again every time
        // it opens, and rearrange itself every time the answer comes back.
        Err(e) if no_such_page(&e) => About::default(),
        Err(e) => return Err(e),
    };
    about.version = CACHE_VERSION;
    if about.screenshots.is_empty() {
        let media = media(namespace);
        about.screenshots = media.pictures;
        about.videos = media.videos;
    }
    if !about.is_bare() {
        about.rating = rating(namespace).unwrap_or_default();
    }
    if let Some(parent) = cached.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(&about) {
        let _ = fs::write(&cached, text);
    }
    Ok(about)
}

/// What the shopfront shows for a game, in the order the store page shows it.
#[derive(Default)]
struct Media {
    pictures: Vec<String>,
    videos: Vec<Clip>,
}

/// The gallery from the shopfront, for a game the content service has no
/// pictures for. Empty when there are none, which is an ordinary answer.
fn media(namespace: &str) -> Media {
    #[derive(Deserialize)]
    struct Response {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "Product")]
        product: Product,
    }
    #[derive(Deserialize)]
    struct Product {
        #[serde(default)]
        sandbox: Option<Sandbox>,
    }
    #[derive(Deserialize)]
    struct Sandbox {
        #[serde(default)]
        configuration: Vec<Configuration>,
    }
    /// The sandbox holds several kinds of configuration and only one of them
    /// is the product page, so the rest arrive empty.
    #[derive(Deserialize)]
    struct Configuration {
        #[serde(default)]
        configs: Option<Configs>,
    }
    #[derive(Deserialize)]
    struct Configs {
        #[serde(default, rename = "keyImages")]
        images: Vec<KeyImage>,
    }
    #[derive(Deserialize)]
    struct KeyImage {
        #[serde(default, rename = "type")]
        kind: String,
        #[serde(default)]
        url: String,
    }

    let response: Response = match super::graphql(
        &super::store_agent(),
        SHOPFRONT,
        MEDIA_QUERY,
        serde_json::json!({ "sandboxId": namespace }),
        None,
        "the store pictures",
    ) {
        Ok(response) => response,
        Err(e) => {
            debug!("no pictures for {namespace}: {e:#}");
            return Media::default();
        }
    };

    let Some(sandbox) = response.data.product.sandbox else {
        return Media::default();
    };
    let images: Vec<KeyImage> = sandbox
        .configuration
        .into_iter()
        .filter_map(|configuration| configuration.configs)
        .flat_map(|configs| configs.images)
        .filter(|image| !image.url.is_empty())
        .collect();

    Media {
        pictures: images
            .iter()
            .filter(|image| GALLERY.contains(&image.kind.as_str()))
            .map(|image| image.url.clone())
            .collect(),
        videos: images
            .iter()
            .filter(|image| image.kind == TRAILER)
            .filter_map(|image| clip(&image.url))
            .collect(),
    }
}

/// Turns one `com.epicgames.video.qs://` URL into something playable.
///
/// The scheme is the store's own and carries two things: the id of the video,
/// which the file has to be asked for by, and the frame the page shows until
/// somebody presses play, which arrives escaped as a parameter.
fn clip(url: &str) -> Option<Clip> {
    let (id, poster) = trailer_parts(url)?;
    Some(Clip {
        url: quicksilver(id)?,
        poster,
    })
}

/// The id and the poster frame inside one of those URLs.
fn trailer_parts(url: &str) -> Option<(&str, String)> {
    let rest = url.strip_prefix("com.epicgames.video.qs://")?;
    let (id, parameters) = match rest.split_once('?') {
        Some((id, parameters)) => (id, parameters),
        None => (rest, ""),
    };
    let poster = parameters
        .split('&')
        .find_map(|parameter| parameter.strip_prefix("cover="))
        .map(unescape)
        .unwrap_or_default();
    Some((id, poster))
}

/// The MP4 behind a QuickSilver id.
fn quicksilver(video_id: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Response {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "Video")]
        video: Video,
    }
    #[derive(Deserialize)]
    struct Video {
        #[serde(default, rename = "fetchQuickSilverVideo")]
        found: Option<Found>,
    }
    #[derive(Deserialize)]
    struct Found {
        #[serde(default)]
        output: Vec<String>,
    }

    let response: Response = super::graphql(
        &super::store_agent(),
        SHOPFRONT,
        VIDEO_QUERY,
        serde_json::json!({ "videoId": video_id }),
        None,
        "the trailer",
    )
    .map_err(|e| debug!("no trailer for {video_id}: {e:#}"))
    .ok()?;

    response
        .data
        .video
        .found?
        .output
        .into_iter()
        .find(|url| url.ends_with(".mp4"))
}

/// Percent decoding, for what the trailer's URL carries as a parameter.
fn unescape(raw: &str) -> String {
    let raw = raw.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        match (raw[at], raw.get(at + 1).copied(), raw.get(at + 2).copied()) {
            (b'%', Some(high), Some(low)) => match (digit(high), digit(low)) {
                (Some(high), Some(low)) => {
                    out.push(high << 4 | low);
                    at += 3;
                    continue;
                }
                // A stray percent sign is a percent sign.
                _ => out.push(b'%'),
            },
            (byte, _, _) => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The store answered, and what it said was that there is no such page.
fn no_such_page(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<ureq::Error>(),
        Some(ureq::Error::StatusCode(404))
    )
}

/// Fetches the store text for a whole library, one game at a time, for the
/// pages nobody has opened yet.
///
/// A page draws itself in one go when the answer is already on disk, and has to
/// hold room for pictures that may not exist when it is not. This is what makes
/// the first case the usual one.
pub fn warm(games: Vec<(String, String)>) {
    static WARMING: AtomicBool = AtomicBool::new(false);
    if WARMING.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        let missing: Vec<_> = games
            .into_iter()
            .filter(|(namespace, _)| !namespace.is_empty() && cached(namespace).is_none())
            .collect();
        if missing.is_empty() {
            WARMING.store(false, Ordering::SeqCst);
            return;
        }
        debug!("reading the store pages of {} games", missing.len());

        let mut failures = 0;
        for (namespace, title) in missing {
            match about(&namespace, &title) {
                Ok(_) => failures = 0,
                Err(e) => {
                    // One game the store cannot answer for is ordinary; three
                    // in a row means there is no store to ask, and the rest of
                    // the library can wait for the next time Divine starts.
                    debug!("cannot read the store page for {title}: {e:#}");
                    failures += 1;
                    if failures == 3 {
                        break;
                    }
                }
            }
            thread::sleep(WARM_PAUSE);
        }
        WARMING.store(false, Ordering::SeqCst);
    });
}

fn product_slug(namespace: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Response {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "Catalog")]
        catalog: Catalog,
    }
    #[derive(Deserialize)]
    struct Catalog {
        #[serde(rename = "catalogNs")]
        catalog_ns: CatalogNs,
    }
    #[derive(Deserialize)]
    struct CatalogNs {
        #[serde(default)]
        mappings: Vec<Mapping>,
    }
    #[derive(Deserialize)]
    struct Mapping {
        #[serde(rename = "pageSlug")]
        page_slug: String,
    }

    let response: Response = super::graphql(
        &super::store_agent(),
        CATALOG,
        SLUG_QUERY,
        serde_json::json!({ "namespace": namespace }),
        None,
        "the store page name",
    )
    .ok()?;
    response
        .data
        .catalog
        .catalog_ns
        .mappings
        .into_iter()
        .next()
        .map(|mapping| mapping.page_slug)
}

/// The store's slugs are the title in lower case with everything else turned
/// into dashes, which is close enough to guess with when the lookup fails.
fn slug_from_title(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn fetch(slug: &str) -> Result<About> {
    #[derive(Deserialize)]
    struct Product {
        #[serde(default)]
        pages: Vec<Page>,
        #[serde(default, rename = "productRatings")]
        product_ratings: ProductRatings,
    }
    #[derive(Default, Deserialize)]
    struct ProductRatings {
        #[serde(default)]
        ratings: Vec<AgeRating>,
    }
    #[derive(Deserialize)]
    struct AgeRating {
        #[serde(default)]
        title: String,
        #[serde(default, rename = "countryCodes")]
        country_codes: String,
        #[serde(default, rename = "contentDescriptors")]
        content_descriptors: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Page {
        #[serde(default, rename = "type")]
        kind: String,
        #[serde(default)]
        data: PageData,
    }
    #[derive(Default, Deserialize)]
    struct PageData {
        #[serde(default)]
        about: RawAbout,
        #[serde(default)]
        carousel: Carousel,
        #[serde(default)]
        meta: Meta,
        #[serde(default)]
        requirements: Requirements,
        #[serde(default, rename = "socialLinks")]
        social_links: HashMap<String, serde_json::Value>,
    }
    #[derive(Default, Deserialize)]
    struct RawAbout {
        #[serde(default)]
        description: String,
        #[serde(default, rename = "shortDescription")]
        short_description: String,
    }
    #[derive(Default, Deserialize)]
    struct Carousel {
        #[serde(default)]
        items: Vec<CarouselItem>,
    }
    #[derive(Deserialize)]
    struct CarouselItem {
        #[serde(default)]
        image: Image,
    }
    #[derive(Default, Deserialize)]
    struct Image {
        #[serde(default)]
        src: String,
    }
    #[derive(Default, Deserialize)]
    struct Meta {
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        publisher: Vec<String>,
    }
    #[derive(Default, Deserialize)]
    struct Requirements {
        #[serde(default)]
        systems: Vec<System>,
        #[serde(default)]
        languages: Vec<String>,
    }
    #[derive(Deserialize)]
    struct System {
        #[serde(default, rename = "systemType")]
        system_type: String,
        #[serde(default)]
        details: Vec<Detail>,
    }
    #[derive(Deserialize)]
    struct Detail {
        #[serde(default)]
        title: String,
        #[serde(default)]
        minimum: String,
        #[serde(default)]
        recommended: String,
    }

    let body = super::store_agent()
        .get(format!("{CONTENT}/{slug}"))
        .call()
        .with_context(|| format!("asking the Epic store about {slug}"))?
        .into_body()
        .read_to_string()
        .context("reading the store page")?;
    let product: Product = serde_json::from_str(&body).context("parsing the store page")?;

    // Age ratings come one per region. The one shown is whichever the store
    // lists first for an English speaking country, since that is the language
    // everything else on the page is in.
    let age = product
        .product_ratings
        .ratings
        .into_iter()
        .find(|rating| rating.country_codes.contains("US"));

    let Some(page) = product
        .pages
        .into_iter()
        .find(|page| page.kind == "productHome")
    else {
        return Ok(About::default());
    };
    let data = page.data;

    // Carousel entries without an image are videos, which are no use here.
    let screenshots = data
        .carousel
        .items
        .into_iter()
        .map(|item| item.image.src)
        .filter(|src| !src.is_empty())
        .collect();

    let requirements = data
        .requirements
        .systems
        .into_iter()
        .find(|system| system.system_type.starts_with("Windows"))
        .map(|system| {
            system
                .details
                .into_iter()
                .map(|detail| Requirement {
                    title: detail.title,
                    minimum: detail.minimum,
                    recommended: detail.recommended,
                })
                .collect()
        })
        .unwrap_or_default();

    let (age_rating, age_descriptors) = match age {
        Some(age) => (
            age.title,
            age.content_descriptors.iter().map(|d| words(d)).collect(),
        ),
        None => (String::new(), Vec::new()),
    };

    Ok(About {
        short: data.about.short_description,
        long: data.about.description,
        screenshots,
        // The trailers on these pages are behind a media service that no
        // longer answers for them, so only the newer pages have any.
        videos: Vec::new(),
        tags: data.meta.tags.iter().map(|tag| words(tag)).collect(),
        links: links(&data.social_links),
        requirements,
        languages: data.requirements.languages,
        publisher: data.meta.publisher.join(", "),
        rating: 0.0,
        age_rating,
        age_descriptors,
        version: 0,
    })
}

/// Epic's player rating, which lives on the store's own GraphQL rather than in
/// the page content.
fn rating(namespace: &str) -> Option<f64> {
    #[derive(Deserialize)]
    struct Response {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "RatingsPolls")]
        ratings: Ratings,
    }
    #[derive(Deserialize)]
    struct Ratings {
        #[serde(default, rename = "getProductResult")]
        result: Option<ProductResult>,
    }
    #[derive(Deserialize)]
    struct ProductResult {
        #[serde(default, rename = "averageRating")]
        average_rating: f64,
    }

    let response: Response = super::graphql(
        &super::store_agent(),
        SHOPFRONT,
        RATING_QUERY,
        serde_json::json!({ "namespace": namespace }),
        None,
        "the store rating",
    )
    .ok()?;
    response.data.ratings.result.map(|r| r.average_rating)
}

/// The store keys its links by name — linkHomepage, linkDiscord — and that name
/// is all there is to label them with.
fn links(raw: &HashMap<String, serde_json::Value>) -> Vec<Link> {
    let mut links: Vec<Link> = raw
        .iter()
        .filter_map(|(key, value)| {
            let name = key.strip_prefix("link")?;
            let url = value.as_str()?;
            if !url.starts_with("http") {
                return None;
            }
            Some(Link {
                title: words(name),
                url: url.to_string(),
            })
        })
        .collect();
    // Homepage first, the rest alphabetically, so the order does not wander.
    links.sort_by_key(|link| (link.title != "Homepage", link.title.clone()));
    links
}

/// "OPEN_WORLD" -> "Open World", "linkHomepage" -> "Homepage". Short all-caps
/// parts are initialisms — RPG, FPS — and stay as they are.
fn words(raw: &str) -> String {
    raw.split(['_', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.len() <= 3 && part.chars().all(|c| c.is_ascii_uppercase()) {
                return part.to_string();
            }
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn older_than(path: &std::path::Path, age: Duration) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|since| since > age)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    /// The three shapes the store really uses, taken from cached answers.
    #[test]
    fn languages_fold_into_the_sections_they_mean() {
        let fold = |lines: &[&str]| {
            super::language_sections(&lines.iter().map(|l| l.to_string()).collect::<Vec<_>>())
        };

        // Control: a heading opens a section and the bare lines join it.
        assert_eq!(
            fold(&[
                "Voice: English",
                "French",
                "German",
                "Text: Polish, Italian",
                "Coming 30th January  2020: Japanese",
            ]),
            [
                ("Voice".to_string(), "English, French, German".to_string()),
                ("Text".to_string(), "Polish, Italian".to_string()),
                (
                    "Coming 30th January  2020".to_string(),
                    "Japanese".to_string()
                ),
            ]
        );

        // Bare lines before any heading are the game's languages outright.
        assert_eq!(
            fold(&["English", "French", "Subtitles only: Arabic, Czech"]),
            [
                ("Languages".to_string(), "English, French".to_string()),
                ("Subtitles only".to_string(), "Arabic, Czech".to_string()),
            ]
        );

        // A shouted heading is calmed down; a sentence one is left alone.
        assert_eq!(fold(&["AUDIO: English"])[0].0, "Audio");
        assert!(fold(&[]).is_empty());
    }

    /// One line, two sections. Every one of these is a real answer, and the
    /// parting between the two sections is different in each.
    #[test]
    fn two_sections_on_one_line_are_read_as_two() {
        let fold = |lines: &[&str]| {
            super::language_sections(&lines.iter().map(|l| l.to_string()).collect::<Vec<_>>())
        };
        let audio_then_text = [
            ("Audio".to_string(), "English".to_string()),
            ("Text".to_string(), "English, French".to_string()),
        ];

        // Discord's, and the shape most of the library uses.
        assert_eq!(
            fold(&["AUDIO: English | TEXT: English, French"]),
            audio_then_text
        );
        // Parted by a run of spaces, and by a comma that reads like one more
        // language until the colon after it says otherwise.
        assert_eq!(
            fold(&["Audio: English     Text: English, French"]),
            audio_then_text
        );
        assert_eq!(
            fold(&["Audio: English, Text: English, French"]),
            audio_then_text
        );
        // A pipe with nothing after it is punctuation, not a language.
        assert_eq!(
            fold(&["AUDIO: English |", "TEXT: English, French"]),
            audio_then_text
        );
        // A space in front of the colon is still the same heading.
        assert_eq!(fold(&["AUDIO : English"])[0].0, "Audio");
    }

    /// Return to Ash's, copied out of the answer that carried it.
    #[test]
    fn a_trailer_url_gives_up_its_id_and_its_poster() {
        let (id, poster) = super::trailer_parts(
            "com.epicgames.video.qs://9546dcd7-a0c9-4b16-8abd-aaacdcdd0f8b\
             ?cover=https%3A%2F%2Fcdn1.epicgames.com%2Fspt-assets%2Freturn-to-ash-video.png",
        )
        .expect("a trailer");
        assert_eq!(id, "9546dcd7-a0c9-4b16-8abd-aaacdcdd0f8b");
        assert_eq!(
            poster,
            "https://cdn1.epicgames.com/spt-assets/return-to-ash-video.png"
        );

        // A trailer with no frame to show is still a trailer.
        assert_eq!(
            super::trailer_parts("com.epicgames.video.qs://9546dcd7"),
            Some(("9546dcd7", String::new()))
        );
        // Anything else in that field is a picture, not a trailer.
        assert!(super::trailer_parts("https://cdn1.epicgames.com/shot.png").is_none());
    }

    /// A percent sign that opens nothing is a percent sign, and the escapes
    /// that do open something are bytes rather than characters.
    #[test]
    fn escapes_come_back_as_what_they_stood_for() {
        assert_eq!(super::unescape("a%20b"), "a b");
        assert_eq!(super::unescape("100%"), "100%");
        assert_eq!(super::unescape("%zz"), "%zz");
        assert_eq!(super::unescape("%D0%9F%D0%BE%D0%B1%D0%B5%D0%B3"), "Побег");
    }

    #[test]
    fn store_names_turn_into_words() {
        assert_eq!(super::words("OPEN_WORLD"), "Open World");
        assert_eq!(super::words("Homepage"), "Homepage");
        assert_eq!(super::words("RPG"), "RPG");
        assert_eq!(super::words("SINGLE_PLAYER"), "Single Player");
    }

    #[test]
    fn a_title_turns_into_a_store_slug() {
        assert_eq!(
            super::slug_from_title("Fallout 3: Game of the Year Edition"),
            "fallout-3-game-of-the-year-edition"
        );
        assert_eq!(super::slug_from_title("Among Us"), "among-us");
    }
}
