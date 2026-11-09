//! An entry in the applications menu for every installed game, each starting
//! the game through `divine --launch` without opening the window.
//!
//! The entries are kept in step with what is installed rather than written
//! once: a game installed, moved or removed has its entry follow. Each carries
//! `X-Divine-AppName`, and only files carrying it are ever rewritten or
//! removed, so an entry somebody wrote by hand under the same name survives.
//!
//! A game whose windows are named after their programs (winewayland) also gets
//! a hidden entry for each further program it has been seen to run, with that
//! program's own name and icon: the Rockstar launcher's window shows Rockstar's
//! icon, the game's shows the game's. A desktop matches a window to an entry by
//! `StartupWMClass` whether the entry is shown in the menu or not, and GNOME
//! and Plasma both match windows again when an entry turns up while they are
//! open, so a program first seen during a session gets its icon in that session.

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
    /// The game's other programs, each to get a hidden entry. Empty unless
    /// windows are named after their programs; otherwise they all share
    /// `window_class` and one entry covers them.
    pub programs: Vec<crate::programs::Program>,
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
            programs: if crate::runner::windows_named_by_program(&installed.app_name, cfg) {
                let main = installed
                    .executable
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or_default()
                    .to_lowercase();
                crate::programs::known(&installed.app_name)
                    .into_iter()
                    .filter(|program| program.class != main)
                    .collect()
            } else {
                Vec::new()
            },
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
        .filter(|installed| stem(&installed.app_name).is_some())
        .map(|installed| Shortcut::new(lgd, cfg, &installed))
        .collect())
}

/// Brings the menu in line with what is installed, or empties it of Divine's
/// entries when they are switched off. Blocking: icons may have to be fetched.
pub fn sync(games: &[Shortcut], enabled: bool) -> Result<()> {
    let wanted = if enabled { games } else { &[] };
    reconcile(wanted, |_| true)
}

/// [`sync`] for one game alone, for a game that has just been seen running a
/// program it was not known to run.
pub fn sync_game(game: &Shortcut) -> Result<()> {
    reconcile(std::slice::from_ref(game), |app_name| {
        app_name == game.app_name
    })
}

/// Writes the entries `games` want and removes the rest of Divine's, among
/// the entries of the games `in_scope` names.
fn reconcile(games: &[Shortcut], in_scope: impl Fn(&str) -> bool) -> Result<()> {
    let dir = applications_dir();
    let ours = existing(&dir);
    let command = command();
    let mut wanted: HashMap<PathBuf, String> = HashMap::new();
    for game in games {
        let Some(stem) = stem(&game.app_name) else {
            continue;
        };
        let icon = log_missing(&game.title, icon(game));
        wanted.insert(
            dir.join(desktop_id(&stem)),
            desktop_entry(game, &command, icon.as_deref()),
        );
        for program in &game.programs {
            let Some(twin) = twin_stem(&stem, program) else {
                continue;
            };
            let icon = log_missing(&program.class, twin_icon(&twin, game, program));
            wanted.insert(
                dir.join(desktop_id(&twin)),
                twin_entry(game, program, &command, icon.as_deref()),
            );
        }
    }

    for (path, app_name) in &ours {
        if in_scope(app_name) && !wanted.contains_key(path) {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
            if let Some(stem) = path
                .file_stem()
                .and_then(|name| name.to_str()?.strip_prefix("divine-"))
            {
                let _ = fs::remove_file(icon_path(stem));
            }
        }
    }

    if wanted.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for (path, entry) in wanted {
        // A file of the same name somebody else wrote is left as it is.
        if path.exists() && !ours.contains_key(&path) {
            continue;
        }
        if fs::read_to_string(&path).ok().as_deref() != Some(entry.as_str()) {
            fs::write(&path, entry).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

fn log_missing(what: &str, icon: Result<PathBuf>) -> Option<PathBuf> {
    icon.inspect_err(|e| warn!("no icon for {what}: {e:#}"))
        .ok()
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
    let stem = stem(&game.app_name).context("the game's name cannot name a file")?;
    make_icon(&stem, game.exe.as_deref(), || {
        warn!("{} has no icon of its own, using its cover", game.title);
        cover_icon(game)
    })
}

/// A program's own icon, or the game's when the program has none.
fn twin_icon(stem: &str, game: &Shortcut, program: &crate::programs::Program) -> Result<PathBuf> {
    make_icon(stem, Some(&program.path), || {
        anyhow::bail!("{} carries no icon", program.class)
    })
    .or_else(|_| icon(game))
}

/// The PNG at `icons/<stem>.png`, made once from the program's icon or, when
/// it has none, from `fallback`.
fn make_icon(
    stem: &str,
    exe: Option<&Path>,
    fallback: impl FnOnce() -> Result<gtk::gdk_pixbuf::Pixbuf>,
) -> Result<PathBuf> {
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
    use gtk::glib;

    let path = icon_path(stem);
    if path.is_file() {
        return Ok(path);
    }
    let dir = icons_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let partial = path.with_extension("partial");

    let from_exe = exe
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
        Err(_) => {
            fallback()?
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

/// The hidden entry for one of a game's other programs. It starts the game
/// like the main entry does, should anything offer to start it, and carries
/// the name the program gives itself.
fn twin_entry(
    game: &Shortcut,
    program: &crate::programs::Program,
    command: &Path,
    icon: Option<&Path>,
) -> String {
    let name = crate::exe_icon::description(&program.path).unwrap_or_else(|| game.title.clone());
    let mut entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Exec={exec} {launch} {app_name}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         StartupWMClass={class}\n\
         {MARK}={app_name}\n",
        name = escape(&name),
        exec = escape(&exec_quote(&command.to_string_lossy())),
        launch = crate::play::LAUNCH,
        app_name = game.app_name,
        class = escape(&program.class),
    );
    if let Some(icon) = icon {
        entry.push_str(&format!("Icon={}\n", escape(&icon.to_string_lossy())));
    }
    entry
}

/// What a game's entry and its icon are filed under. Desktop file ids are
/// letters, digits, `-` and `_`, and a game whose app name strays outside that
/// gets none rather than one that has to be decoded back.
fn stem(app_name: &str) -> Option<String> {
    let fits = !app_name.is_empty()
        && app_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    fits.then(|| app_name.to_string())
}

/// What one of a game's other programs is filed under: the game's stem, two
/// dashes, and the program's file name without what cannot go in an id.
fn twin_stem(game: &str, program: &crate::programs::Program) -> Option<String> {
    let name: String = program
        .class
        .trim_end_matches(".exe")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then(|| format!("{game}--{name}"))
}

fn desktop_id(stem: &str) -> String {
    format!("divine-{stem}.desktop")
}

/// Divine's entries already in the menu, each with the game it is for.
fn existing(dir: &Path) -> HashMap<PathBuf, String> {
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
            ours.insert(path, app_name.trim().to_string());
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

fn icon_path(stem: &str) -> PathBuf {
    icons_dir().join(format!("{stem}.png"))
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
            programs: Vec::new(),
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
        assert_eq!(stem("Fowl").as_deref(), Some("Fowl"));
        assert_eq!(
            stem("963137e4c29d4c79a81323b8fab03a40").as_deref(),
            Some("963137e4c29d4c79a81323b8fab03a40")
        );
        assert!(stem("../evil").is_none());
        assert!(stem("").is_none());
    }

    /// The Rockstar launcher GTA V starts, as programs.rs writes it down.
    fn launcher() -> crate::programs::Program {
        crate::programs::Program {
            class: "launcher.exe".into(),
            path: "/nowhere/Rockstar Games/Launcher/Launcher.exe".into(),
        }
    }

    #[test]
    fn another_program_gets_a_hidden_entry_its_window_will_match() {
        let gta = game(
            "8769e24080ea413b8ebca3f1b8c50951",
            "Grand Theft Auto V Enhanced",
        );
        let twin = twin_stem("8769e24080ea413b8ebca3f1b8c50951", &launcher()).unwrap();
        assert_eq!(twin, "8769e24080ea413b8ebca3f1b8c50951--launcher");
        let entry = twin_entry(&gta, &launcher(), Path::new("divine"), None);
        assert!(entry.contains("\nNoDisplay=true\n"));
        assert!(entry.contains("\nStartupWMClass=launcher.exe\n"));
        // Still the game's, so it is kept and removed along with the game.
        assert!(entry.contains("\nX-Divine-AppName=8769e24080ea413b8ebca3f1b8c50951\n"));
        // A program that does not say what it is goes by the game's name.
        assert!(entry.contains("\nName=Grand Theft Auto V Enhanced\n"));
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
        assert_eq!(
            ours.get(&dir.join("divine-Fowl.desktop"))
                .map(String::as_str),
            Some("Fowl")
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
