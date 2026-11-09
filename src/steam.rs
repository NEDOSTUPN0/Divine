//! Games added to Steam as non-Steam shortcuts, so they show up in the Steam
//! library and in its game mode with art of their own.
//!
//! A shortcut runs `divine --launch`, which runs the game as its own child and
//! waits for it: Steam sees the game for exactly as long as it is up, and its
//! overlay and game mode treat it like any other title.
//!
//! Steam keeps its shortcuts in `userdata/<user>/config/shortcuts.vdf`, a
//! binary key-value file, and reads it when it starts. Divine only writes it
//! while Steam is closed: a running Steam holds its own copy of the list, and
//! what it would make of the file changing underneath it is not worth finding
//! out on somebody's library.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::shortcuts::Shortcut;

/// Where Steam lives, in the order the usual installs put it.
fn root() -> Option<PathBuf> {
    let home = crate::config::home();
    [
        home.join(".steam/root"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
    ]
    .into_iter()
    .find(|dir| dir.join("userdata").is_dir())
}

/// Whether there is a Steam to add games to at all.
pub fn found() -> bool {
    root().is_some()
}

/// Whether Steam is up, by its process name.
pub fn running() -> bool {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|comm| comm.trim_end() == "steam")
        })
}

/// The `config` directory of the Steam user who signed in last, which is the
/// one whose library a shortcut is meant for.
fn user_config() -> Result<PathBuf> {
    let root = root().context("Steam is not installed")?;
    let userdata = root.join("userdata");
    let users: Vec<PathBuf> = fs::read_dir(&userdata)
        .with_context(|| format!("reading {}", userdata.display()))?
        .flatten()
        .map(|entry| entry.path())
        // "0" is where Steam keeps what belongs to nobody in particular.
        .filter(|dir| dir.is_dir() && dir.file_name().is_some_and(|name| name != "0"))
        .collect();

    let recent = fs::read_to_string(root.join("config/loginusers.vdf"))
        .ok()
        .and_then(|text| most_recent_user(&text))
        .map(|id| userdata.join(id.to_string()));
    let user = match recent {
        Some(dir) if dir.is_dir() => dir,
        _ => match users.as_slice() {
            [only] => only.clone(),
            [] => bail!("nobody has signed in to Steam on this machine yet"),
            _ => bail!("cannot tell which Steam user signed in last"),
        },
    };
    Ok(user.join("config"))
}

/// The account id of the user `loginusers.vdf` marks `MostRecent`. Steam lists
/// users by their 64-bit id and names their folders by the 32-bit account id
/// inside it.
fn most_recent_user(text: &str) -> Option<u32> {
    const INDIVIDUAL_BASE: u64 = 76_561_197_960_265_728;
    let tokens = text_tokens(text);
    let mut current: Option<u64> = None;
    let mut depth = 0;
    let mut tokens = tokens.iter().peekable();
    while let Some(token) = tokens.next() {
        match token.as_str() {
            "{" => depth += 1,
            "}" => depth -= 1,
            key if depth == 1 && tokens.peek().is_some_and(|next| *next == "{") => {
                current = key.parse().ok();
            }
            "MostRecent" if depth == 2 && tokens.next().is_some_and(|value| value == "1") => {
                let id = current?.checked_sub(INDIVIDUAL_BASE)?;
                return u32::try_from(id).ok();
            }
            _ => {}
        }
    }
    None
}

/// Steam's text format, reduced to its tokens: quoted strings and braces.
fn text_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' => tokens.push(c.to_string()),
            '"' => {
                let mut token = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => token.extend(chars.next()),
                        c => token.push(c),
                    }
                }
                tokens.push(token);
            }
            _ => {}
        }
    }
    tokens
}

/// Whether the game is in Steam already.
pub fn has(app_name: &str) -> bool {
    let Ok(config) = user_config() else {
        return false;
    };
    read_shortcuts(&config.join("shortcuts.vdf"))
        .is_ok_and(|list| list.iter().any(|entry| is_for(entry, app_name)))
}

/// Adds the game to Steam, or brings its entry up to date, with art.
pub fn add(game: &Shortcut) -> Result<()> {
    if running() {
        bail!("Steam is running. Close it, then try again.");
    }
    let config = user_config()?;
    let file = config.join("shortcuts.vdf");
    let mut list = read_shortcuts(&file)?;

    let exe = format!("\"{}\"", crate::shortcuts::command().display());
    let appid = shortcut_id(&exe, &game.title);
    let grid = config.join("grid");
    fs::create_dir_all(&grid).with_context(|| format!("creating {}", grid.display()))?;
    let icon = match crate::shortcuts::icon(game) {
        Ok(icon) => icon.display().to_string(),
        Err(e) => {
            crate::log::warn!("no icon for {}: {e:#}", game.title);
            String::new()
        }
    };
    save_art(game, &grid, appid);

    list.retain(|entry| !is_for(entry, &game.app_name));
    list.push(entry(game, appid, &exe, &icon));
    write_shortcuts(&file, &list)
}

/// Takes the game out of Steam, and the art Divine put there for it.
pub fn remove(app_name: &str) -> Result<()> {
    if running() {
        bail!("Steam is running. Close it, then try again.");
    }
    let config = user_config()?;
    let file = config.join("shortcuts.vdf");
    let mut list = read_shortcuts(&file)?;
    let mut gone = Vec::new();
    list.retain(|entry| {
        if !is_for(entry, app_name) {
            return true;
        }
        if let Some(Value::Int(appid)) = field(entry, "appid") {
            gone.push(*appid);
        }
        false
    });
    write_shortcuts(&file, &list)?;
    for appid in gone {
        for name in art_names(appid) {
            for ext in ["png", "jpg"] {
                crate::shortcuts::remove_if_there(
                    &config.join("grid").join(format!("{name}.{ext}")),
                )?;
            }
        }
    }
    Ok(())
}

/// The id Steam files a shortcut's art under. Steam takes whatever id the file
/// gives; this is the one other tools give for the same program and name,
/// so adding a game twice lands on the same art.
fn shortcut_id(exe: &str, name: &str) -> u32 {
    crc32(format!("{exe}{name}").as_bytes()) | 0x8000_0000
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The grid names Steam looks for, without the extension: the portrait
/// capsule, the wide one, and the hero banner behind the game's page.
fn art_names(appid: u32) -> [String; 3] {
    [
        format!("{appid}p"),
        format!("{appid}"),
        format!("{appid}_hero"),
    ]
}

/// Downloads Epic's art into Steam's grid folder. Art is a nicety: a game
/// that has none, or whose art does not arrive, is still added.
fn save_art(game: &Shortcut, grid: &Path, appid: u32) {
    let [portrait, wide, hero] = art_names(appid);
    let tall = game
        .image("DieselGameBoxTall")
        .or_else(|| game.image("OfferImageTall"));
    let landscape = game
        .image("DieselGameBox")
        .or_else(|| game.image("OfferImageWide"));
    let wanted = [
        (tall, "?h=900&w=675&resize=1", portrait),
        (landscape, "?h=430&w=920&resize=1", wide),
        (landscape, "?h=1080&w=1920&resize=1", hero),
    ];
    for (image, size, name) in wanted {
        let Some(image) = image else {
            continue;
        };
        if let Err(e) = crate::covers::save(&image.url, size, &grid.join(name)) {
            crate::log::warn!("no Steam art for {}: {e:#}", game.title);
        }
    }
}

/// One shortcut, with the fields Steam writes for one it made itself, in the
/// same order. Read off a `shortcuts.vdf` written by Steam.
fn entry(game: &Shortcut, appid: u32, exe: &str, icon: &str) -> Fields {
    let text = |value: &str| Value::Str(value.as_bytes().to_vec());
    let start_dir = crate::shortcuts::command()
        .parent()
        .map(|dir| format!("\"{}\"", dir.display()))
        .unwrap_or_default();
    [
        ("appid", Value::Int(appid)),
        ("AppName", text(&game.title)),
        ("Exe", text(exe)),
        ("StartDir", text(&start_dir)),
        ("icon", text(icon)),
        ("ShortcutPath", text("")),
        ("LaunchOptions", text(&launch_options(&game.app_name))),
        ("IsHidden", Value::Int(0)),
        ("AllowDesktopConfig", Value::Int(1)),
        ("AllowOverlay", Value::Int(1)),
        ("OpenVR", Value::Int(0)),
        ("Devkit", Value::Int(0)),
        ("DevkitGameID", text("")),
        ("DevkitOverrideAppID", Value::Int(0)),
        ("LastPlayTime", Value::Int(0)),
        ("FlatpakAppID", text("")),
        ("sortas", text("")),
        ("tags", Value::Map(Vec::new())),
    ]
    .into_iter()
    .map(|(key, value)| (key.as_bytes().to_vec(), value))
    .collect()
}

fn launch_options(app_name: &str) -> String {
    format!("{} {app_name}", crate::play::LAUNCH)
}

/// Whether a shortcut is the one Divine made for this game. Recognised by what
/// it runs rather than by name, which the user may have changed in Steam.
fn is_for(entry: &[(Vec<u8>, Value)], app_name: &str) -> bool {
    matches!(field(entry, "LaunchOptions"), Some(Value::Str(options))
        if options.as_slice() == launch_options(app_name).as_bytes())
}

fn field<'a>(entry: &'a [(Vec<u8>, Value)], key: &str) -> Option<&'a Value> {
    entry
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key.as_bytes()))
        .map(|(_, value)| value)
}

/// A map's entries in the order the file has them, keys as raw bytes.
type Fields = Vec<(Vec<u8>, Value)>;

/// One value in Steam's binary key-value format.
///
/// Strings are kept as bytes: Steam does not promise UTF-8, and a shortcut
/// somebody else made has to be written back exactly as it was read.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Map(Fields),
    Str(Vec<u8>),
    Int(u32),
    /// The types shortcuts never use, carried through untouched.
    Other(u8, Vec<u8>),
}

const MAP: u8 = 0x00;
const STR: u8 = 0x01;
const INT: u8 = 0x02;
const END: u8 = 0x08;

/// The shortcuts in the file, each as its fields. No file is no shortcuts.
fn read_shortcuts(file: &Path) -> Result<Vec<Fields>> {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let mut at = 0;
    let root = read_map(&bytes, &mut at).with_context(|| format!("reading {}", file.display()))?;
    let Some(Value::Map(list)) = root
        .into_iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(b"shortcuts"))
        .map(|(_, value)| value)
    else {
        return Ok(Vec::new());
    };
    Ok(list
        .into_iter()
        .filter_map(|(_, value)| match value {
            Value::Map(fields) => Some(fields),
            _ => None,
        })
        .collect())
}

/// Writes the list back, numbering the entries afresh as Steam does.
fn write_shortcuts(file: &Path, list: &[Fields]) -> Result<()> {
    let numbered = list
        .iter()
        .enumerate()
        .map(|(index, fields)| (index.to_string().into_bytes(), Value::Map(fields.clone())))
        .collect();
    let mut bytes = Vec::new();
    write_map(&[(b"shortcuts".to_vec(), Value::Map(numbered))], &mut bytes);
    let partial = file.with_extension("partial");
    fs::write(&partial, &bytes).with_context(|| format!("writing {}", partial.display()))?;
    fs::rename(&partial, file).with_context(|| format!("writing {}", file.display()))
}

fn read_map(bytes: &[u8], at: &mut usize) -> Result<Fields> {
    let mut map = Vec::new();
    loop {
        let kind = *bytes.get(*at).context("the file ends in the middle")?;
        *at += 1;
        if kind == END {
            return Ok(map);
        }
        let key = read_text(bytes, at)?;
        let value = match kind {
            MAP => Value::Map(read_map(bytes, at)?),
            STR => Value::Str(read_text(bytes, at)?),
            INT => Value::Int(u32::from_le_bytes(take(bytes, at, 4)?.try_into()?)),
            // Float, pointer and colour are four bytes; the two 64-bit
            // integers eight.
            0x03 | 0x04 | 0x06 => Value::Other(kind, take(bytes, at, 4)?.to_vec()),
            0x07 | 0x0A => Value::Other(kind, take(bytes, at, 8)?.to_vec()),
            other => bail!("a value of type {other:#04x}, which this reader does not know"),
        };
        map.push((key, value));
    }
}

fn read_text(bytes: &[u8], at: &mut usize) -> Result<Vec<u8>> {
    let rest = bytes.get(*at..).context("the file ends in the middle")?;
    let len = rest
        .iter()
        .position(|byte| *byte == 0)
        .context("a string runs off the end of the file")?;
    *at += len + 1;
    Ok(rest[..len].to_vec())
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, len: usize) -> Result<&'a [u8]> {
    let slice = bytes
        .get(*at..*at + len)
        .context("the file ends in the middle")?;
    *at += len;
    Ok(slice)
}

fn write_map(map: &[(Vec<u8>, Value)], out: &mut Vec<u8>) {
    for (key, value) in map {
        let kind = match value {
            Value::Map(_) => MAP,
            Value::Str(_) => STR,
            Value::Int(_) => INT,
            Value::Other(kind, _) => *kind,
        };
        out.push(kind);
        out.extend_from_slice(key);
        out.push(0);
        match value {
            Value::Map(inner) => write_map(inner, out),
            Value::Str(text) => {
                out.extend_from_slice(text);
                out.push(0);
            }
            Value::Int(number) => out.extend_from_slice(&number.to_le_bytes()),
            Value::Other(_, raw) => out.extend_from_slice(raw),
        }
    }
    out.push(END);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shortcuts.vdf Steam wrote for a game added through its own dialog,
    /// with the paths shortened.
    const STEAMS_OWN: &[u8] = b"\x00shortcuts\x00\x000\x00\x02appid\x00\x9f\xde\xaa\xb4\
\x01AppName\x00Forza Horizon 5\x00\x01Exe\x00/Games/FH5/ForzaHorizon5.exe\x00\
\x01StartDir\x00/Games/FH5/\x00\x01icon\x00/Games/FH5/icon.png\x00\x01ShortcutPath\x00\x00\
\x01LaunchOptions\x00PROTON_LOG=1 %command%\x00\x02IsHidden\x00\x00\x00\x00\x00\
\x02AllowDesktopConfig\x00\x01\x00\x00\x00\x02AllowOverlay\x00\x01\x00\x00\x00\
\x02OpenVR\x00\x00\x00\x00\x00\x02Devkit\x00\x00\x00\x00\x00\x01DevkitGameID\x00\x00\
\x02DevkitOverrideAppID\x00\x00\x00\x00\x00\x02LastPlayTime\x00\xe9\xfe\xe7i\
\x01FlatpakAppID\x00\x00\x01sortas\x00\x00\x00tags\x00\x08\x08\x08\x08";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("divine-steam-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_steam_wrote_comes_back_byte_for_byte() {
        let dir = scratch("roundtrip");
        let file = dir.join("shortcuts.vdf");
        fs::write(&file, STEAMS_OWN).unwrap();
        let list = read_shortcuts(&file).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            field(&list[0], "AppName"),
            Some(&Value::Str(b"Forza Horizon 5".to_vec()))
        );
        write_shortcuts(&file, &list).unwrap();
        assert_eq!(fs::read(&file).unwrap(), STEAMS_OWN);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adding_a_game_leaves_somebody_elses_shortcut_alone() {
        let dir = scratch("keep");
        let file = dir.join("shortcuts.vdf");
        fs::write(&file, STEAMS_OWN).unwrap();
        let game = Shortcut {
            app_name: "Fowl".into(),
            title: "The Escapists 2".into(),
            key_images: Vec::new(),
            exe: None,
            window_class: None,
        };

        for _ in 0..2 {
            let mut list = read_shortcuts(&file).unwrap();
            list.retain(|entry| !is_for(entry, "Fowl"));
            list.push(entry(&game, 0x8000_0001, "\"/bin/divine\"", ""));
            write_shortcuts(&file, &list).unwrap();
        }

        let list = read_shortcuts(&file).unwrap();
        // Twice added is still once there, and Forza is untouched.
        assert_eq!(list.len(), 2);
        assert!(is_for(&list[1], "Fowl"));
        assert!(!is_for(&list[0], "Fowl"));
        assert_eq!(
            field(&list[0], "LaunchOptions"),
            Some(&Value::Str(b"PROTON_LOG=1 %command%".to_vec()))
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_shortcut_list_is_read_as_one() {
        let dir = scratch("empty");
        let file = dir.join("shortcuts.vdf");
        // As Steam leaves it for a user with no shortcuts.
        fs::write(&file, b"\x00shortcuts\x00\x08\x08").unwrap();
        assert!(read_shortcuts(&file).unwrap().is_empty());
        assert!(read_shortcuts(&dir.join("absent.vdf")).unwrap().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_truncated_file_is_refused_rather_than_rewritten() {
        let dir = scratch("truncated");
        let file = dir.join("shortcuts.vdf");
        fs::write(&file, &STEAMS_OWN[..60]).unwrap();
        assert!(read_shortcuts(&file).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_user_who_signed_in_last_is_found() {
        let text = r#""users"
{
	"76561199000067455"
	{
		"AccountName"		"someone"
		"MostRecent"		"0"
	}
	"76561198000000001"
	{
		"AccountName"		"someone else"
		"MostRecent"		"1"
	}
}"#;
        assert_eq!(most_recent_user(text), Some(39_734_273));
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
