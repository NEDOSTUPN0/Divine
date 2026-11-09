//! An entry in the applications menu for every installed game, each starting
//! the game through `divine --launch` without opening the window.
//!
//! The entries are kept in step with what is installed rather than written
//! once: a game installed, moved or removed has its entry follow. Each carries
//! `X-Divine-AppName`, and only files carrying it are ever rewritten or
//! removed, so an entry somebody wrote by hand under the same name survives.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::legendary::Legendary;
use crate::log::warn;
use crate::models::{Cover, InstalledEntry, KeyImage};

/// The key that marks an entry as Divine's.
const MARK: &str = "X-Divine-AppName";
/// What an icon made from a cover is drawn at: big enough for any menu or dock.
const ICON_SIZE: i32 = 256;

/// What a shortcut needs to know about one installed game.
#[derive(Debug, Clone)]
pub struct Shortcut {
    pub app_name: String,
    pub title: String,
    pub key_images: Vec<KeyImage>,
    /// The game's own program, whose icon is the one to show.
    pub exe: Option<PathBuf>,
    /// What its window will be called, so the desktop can tell that a running
    /// window is this game's and show this entry's icon on it.
    pub window_class: Option<String>,
}

impl Shortcut {
    /// Blocking: working out the window's name may ask umu's database.
    pub fn new(lgd: &Legendary, cfg: &Config, installed: &InstalledEntry) -> Self {
        let cached = lgd.cached_entry(&installed.app_name);
        Shortcut {
            title: if installed.title.is_empty() {
                cached
                    .as_ref()
                    .map_or(installed.app_name.clone(), |c| c.app_title.clone())
            } else {
                installed.title.clone()
            },
            key_images: cached.map(|c| c.metadata.key_images).unwrap_or_default(),
            exe: (!installed.executable.is_empty())
                .then(|| Path::new(&installed.install_path).join(&installed.executable)),
            window_class: crate::runner::window_class(
                &installed.app_name,
                &installed.executable,
                cfg,
            ),
            app_name: installed.app_name.clone(),
        }
    }

    /// Epic's crop of a given shape, when it has one.
    pub fn image(&self, kind: &str) -> Option<&KeyImage> {
        self.key_images.iter().find(|image| image.kind == kind)
    }
}

/// Every installed game, whichever account owns it: a game installed under
/// another account is still on the disk and still worth starting.
pub fn installed(lgd: &Legendary, cfg: &Config) -> Result<Vec<Shortcut>> {
    Ok(lgd
        .list_installed()
        .context("listing the installed games")?
        .into_iter()
        .filter(|installed| desktop_id(&installed.app_name).is_some())
        .map(|installed| Shortcut::new(lgd, cfg, &installed))
        .collect())
}

/// Brings the menu in line with what is installed, or empties it of Divine's
/// entries when they are switched off. Blocking: icons may have to be fetched.
pub fn sync(games: &[Shortcut], enabled: bool) -> Result<()> {
    let dir = applications_dir();
    let ours = existing(&dir);
    let wanted: HashMap<&str, &Shortcut> = if enabled {
        games
            .iter()
            .map(|game| (game.app_name.as_str(), game))
            .collect()
    } else {
        HashMap::new()
    };

    for (app_name, path) in &ours {
        if !wanted.contains_key(app_name.as_str()) {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
            let _ = fs::remove_file(icon_path(app_name));
        }
    }

    if wanted.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let command = command();
    for game in wanted.values() {
        let Some(id) = desktop_id(&game.app_name) else {
            continue;
        };
        let path = dir.join(id);
        // A file of the same name somebody else wrote is left as it is.
        if path.exists() && !ours.contains_key(&game.app_name) {
            continue;
        }
        let icon = match icon(game) {
            Ok(icon) => Some(icon),
            Err(e) => {
                warn!("no icon for {}: {e:#}", game.title);
                None
            }
        };
        let entry = desktop_entry(game, &command, icon.as_deref());
        if fs::read_to_string(&path).ok().as_deref() != Some(entry.as_str()) {
            fs::write(&path, entry).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

/// The program a shortcut runs. Found on `PATH` where possible, because the
/// path of this very binary may be one the next update removes: a store path
/// under Nix, a build directory while developing.
pub fn command() -> PathBuf {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("divine"))
            .find(|candidate| is_executable(candidate))
    });
    on_path
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("divine"))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The game's icon as a PNG of its own: the one in its program, as Windows
/// and Steam would show it, or failing that its cover.
pub fn icon(game: &Shortcut) -> Result<PathBuf> {
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
    use gtk::glib;

    let path = icon_path(&game.app_name);
    if path.is_file() {
        return Ok(path);
    }
    let dir = icons_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let partial = path.with_extension("partial");

    let from_exe = game
        .exe
        .as_deref()
        .context("legendary does not know the game's program")
        .and_then(crate::exe_icon::read);
    match from_exe {
        Ok(crate::exe_icon::Icon::Png(bytes)) => {
            fs::write(&partial, bytes).with_context(|| format!("writing {}", partial.display()))?;
        }
        Ok(crate::exe_icon::Icon::Rgba {
            width,
            height,
            pixels,
        }) => {
            let width = width as i32;
            Pixbuf::from_bytes(
                &glib::Bytes::from_owned(pixels),
                Colorspace::Rgb,
                true,
                8,
                width,
                height as i32,
                width * 4,
            )
            .savev(&partial, "png", &[])
            .with_context(|| format!("writing {}", partial.display()))?;
        }
        Err(e) => {
            warn!(
                "{} has no icon of its own, using its cover: {e:#}",
                game.title
            );
            cover_icon(game)?
                .savev(&partial, "png", &[])
                .with_context(|| format!("writing {}", partial.display()))?;
        }
    }
    fs::rename(&partial, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The portrait cover centred on a transparent square: Epic has no square art
/// for most games, and a crop would cut the title off.
fn cover_icon(game: &Shortcut) -> Result<gtk::gdk_pixbuf::Pixbuf> {
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};

    let image = game
        .image("DieselGameBoxTall")
        .or_else(|| game.image("OfferImageTall"))
        .context("Epic has no portrait art for this game")?;
    let cover = crate::covers::fetch(&Cover {
        url: image.url.clone(),
        md5: image.md5.clone(),
    })?;

    let art = Pixbuf::from_file_at_scale(&cover, ICON_SIZE, ICON_SIZE, true)
        .with_context(|| format!("reading {}", cover.display()))?;
    let square = Pixbuf::new(Colorspace::Rgb, true, 8, ICON_SIZE, ICON_SIZE)
        .context("making room for the icon")?;
    square.fill(0);
    art.copy_area(
        0,
        0,
        art.width(),
        art.height(),
        &square,
        (ICON_SIZE - art.width()) / 2,
        (ICON_SIZE - art.height()) / 2,
    );
    Ok(square)
}

fn desktop_entry(game: &Shortcut, command: &Path, icon: Option<&Path>) -> String {
    let mut entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Comment=Play {name} through Divine\n\
         Exec={exec} {launch} {app_name}\n\
         Terminal=false\n\
         Categories=Game;\n\
         {MARK}={app_name}\n",
        name = escape(&game.title),
        exec = escape(&exec_quote(&command.to_string_lossy())),
        launch = crate::play::LAUNCH,
        app_name = game.app_name,
    );
    if let Some(icon) = icon {
        entry.push_str(&format!("Icon={}\n", escape(&icon.to_string_lossy())));
    }
    if let Some(class) = &game.window_class {
        entry.push_str(&format!("StartupWMClass={}\n", escape(class)));
    }
    entry
}

/// The file name of a game's entry. Desktop file ids are letters, digits,
/// `-` and `_`, and a game whose app name strays outside that gets none rather
/// than one that has to be decoded back.
fn desktop_id(app_name: &str) -> Option<String> {
    let fits = !app_name.is_empty()
        && app_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    fits.then(|| format!("divine-{app_name}.desktop"))
}

/// Divine's entries already in the menu, by the game each is for.
fn existing(dir: &Path) -> HashMap<String, PathBuf> {
    let mut ours = HashMap::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "desktop") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let app_name = text
            .lines()
            .find_map(|line| line.strip_prefix(MARK)?.strip_prefix('='));
        if let Some(app_name) = app_name {
            ours.insert(app_name.trim().to_string(), path);
        }
    }
    ours
}

/// A string value as the desktop entry specification wants it written.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// Quotes a program path for `Exec`, which has its own rules on top of the
/// escaping every value gets: inside double quotes, `"`, `` ` ``, `$` and `\`
/// take a backslash.
fn exec_quote(arg: &str) -> String {
    let plain = arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c));
    if plain {
        return arg.to_string();
    }
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        if "\"`$\\".contains(c) {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

fn applications_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| crate::config::home().join(".local/share"))
        .join("applications")
}

fn icons_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| crate::config::home().join(".local/share"))
        .join("divine")
        .join("icons")
}

fn icon_path(app_name: &str) -> PathBuf {
    icons_dir().join(format!("{app_name}.png"))
}

/// Removes a file Divine made, where having nothing to remove is fine.
pub fn remove_if_there(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(app_name: &str, title: &str) -> Shortcut {
        Shortcut {
            app_name: app_name.into(),
            title: title.into(),
            key_images: Vec::new(),
            exe: None,
            window_class: Some("steam_app_egs_Fowl".into()),
        }
    }

    #[test]
    fn an_entry_starts_the_game_without_the_window() {
        let entry = desktop_entry(
            &game("Fowl", "The Escapists 2"),
            Path::new("/home/someone/.nix-profile/bin/divine"),
            Some(Path::new(
                "/home/someone/.local/share/divine/icons/Fowl.png",
            )),
        );
        assert!(entry.contains("\nExec=/home/someone/.nix-profile/bin/divine --launch Fowl\n"));
        assert!(entry.contains("\nName=The Escapists 2\n"));
        assert!(entry.contains("\nX-Divine-AppName=Fowl\n"));
        // How a running window finds its way back to this entry and its icon.
        assert!(entry.contains("\nStartupWMClass=steam_app_egs_Fowl\n"));
    }

    #[test]
    fn a_path_with_spaces_survives_the_exec_rules() {
        let entry = desktop_entry(
            &game("Fowl", "The Escapists 2"),
            Path::new("/opt/My Games/divine"),
            None,
        );
        assert!(entry.contains("\nExec=\"/opt/My Games/divine\" --launch Fowl\n"));
        assert_eq!(exec_quote("/a/$HOME"), "\"/a/\\$HOME\"");
        // And the escaping every value gets comes on top of that.
        assert_eq!(escape(&exec_quote("/a b/\\x")), "\"/a b/\\\\\\\\x\"");
    }

    #[test]
    fn an_app_name_that_cannot_be_a_file_name_gets_no_entry() {
        assert_eq!(desktop_id("Fowl").as_deref(), Some("divine-Fowl.desktop"));
        assert_eq!(
            desktop_id("963137e4c29d4c79a81323b8fab03a40").as_deref(),
            Some("divine-963137e4c29d4c79a81323b8fab03a40.desktop")
        );
        assert!(desktop_id("../evil").is_none());
        assert!(desktop_id("").is_none());
    }

    #[test]
    fn only_entries_divine_marked_are_counted_as_its_own() {
        let dir = std::env::temp_dir().join(format!("divine-menu-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("divine-Fowl.desktop"),
            desktop_entry(&game("Fowl", "The Escapists 2"), Path::new("divine"), None),
        )
        .unwrap();
        // Same naming scheme, written by hand.
        fs::write(
            dir.join("divine-Kinglet.desktop"),
            "[Desktop Entry]\nName=Mine\nExec=true\n",
        )
        .unwrap();

        let ours = existing(&dir);
        assert_eq!(ours.len(), 1);
        assert!(ours.contains_key("Fowl"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
