//! Turning a game's gamescope settings into gamescope's command line.
//!
//! gamescope wraps everything from umu inwards and takes the game after a
//! `--`. The option and value names are those gamescope 3.17 prints in
//! `--help`. Anything left unset is left out rather than written as what the
//! default is believed to be, so gamescope's own defaults stay in charge.

use anyhow::{Context, Result};

use crate::config::{Gamescope, GamescopeFilter, GamescopeScaler, GamescopeWindow};

pub const BINARY: &str = "gamescope";

/// Whether a game will start inside gamescope: switched on, and there to run.
pub fn in_use(settings: &Gamescope) -> bool {
    settings.enabled && crate::runner::installed(BINARY)
}

/// MangoHud built to run beside gamescope rather than inside the game.
/// gamescope's help says to use it in place of mangohud on the game.
pub const MANGOAPP: &str = "mangoapp";

/// gamescope and its options, ending in the `--` that the game follows.
pub fn command(settings: &Gamescope, mangoapp: bool) -> Result<Vec<String>> {
    let mut args = vec![BINARY.to_string()];
    let mut number = |flag: &str, value: u32| {
        if value > 0 {
            args.push(flag.into());
            args.push(value.to_string());
        }
    };
    number("-w", settings.game_width);
    number("-h", settings.game_height);
    number("-W", settings.output_width);
    number("-H", settings.output_height);
    number("-r", settings.refresh_rate);

    match settings.window {
        GamescopeWindow::Windowed => {}
        GamescopeWindow::Borderless => args.push("-b".into()),
        GamescopeWindow::Fullscreen => args.push("-f".into()),
    }
    if let Some(scaler) = settings.scaler {
        args.push("-S".into());
        args.push(scaler_name(scaler).into());
    }
    if let Some(filter) = settings.filter {
        args.push("-F".into());
        args.push(filter_name(filter).into());
    }
    if let Some(sharpness) = settings.sharpness {
        args.push("--sharpness".into());
        args.push(sharpness.to_string());
    }
    if settings.hdr {
        args.push("--hdr-enabled".into());
    }
    if settings.grab_keyboard {
        args.push("-g".into());
    }
    if settings.force_grab_cursor {
        args.push("--force-grab-cursor".into());
    }
    if mangoapp {
        args.push("--mangoapp".into());
    }
    args.extend(
        crate::launch_options::split(&settings.extra_args)
            .context("reading the extra gamescope arguments")?,
    );
    args.push("--".into());
    Ok(args)
}

fn scaler_name(scaler: GamescopeScaler) -> &'static str {
    match scaler {
        GamescopeScaler::Auto => "auto",
        GamescopeScaler::Integer => "integer",
        GamescopeScaler::Fit => "fit",
        GamescopeScaler::Fill => "fill",
        GamescopeScaler::Stretch => "stretch",
    }
}

fn filter_name(filter: GamescopeFilter) -> &'static str {
    match filter {
        GamescopeFilter::Linear => "linear",
        GamescopeFilter::Nearest => "nearest",
        GamescopeFilter::Fsr => "fsr",
        GamescopeFilter::Nis => "nis",
        GamescopeFilter::Pixel => "pixel",
        GamescopeFilter::Sgsr => "sgsr",
    }
}

/// A resolution typed as `1920x1080`. Empty means none; anything else that is
/// not two positive numbers is `None`, for the row to mark as wrong.
pub fn parse_resolution(text: &str) -> Option<(u32, u32)> {
    let text = text.trim();
    if text.is_empty() {
        return Some((0, 0));
    }
    let (width, height) = text.split_once(['x', 'X', '×'])?;
    let width: u32 = width.trim().parse().ok()?;
    let height: u32 = height.trim().parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// The other way round; empty when nothing is set.
pub fn show_resolution(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        String::new()
    } else {
        format!("{width}x{height}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_chosen_passes_nothing_but_the_separator() {
        assert_eq!(
            command(&Gamescope::default(), false).expect("building"),
            ["gamescope", "--"]
        );
    }

    /// `-h` here is gamescope's game height, not a call for help: it has to
    /// reach gamescope next to `-w`, with the game only after the `--`.
    #[test]
    fn every_setting_lands_on_its_own_flag() {
        let settings = Gamescope {
            enabled: true,
            game_width: 1280,
            game_height: 720,
            output_width: 2560,
            output_height: 1440,
            refresh_rate: 144,
            window: GamescopeWindow::Fullscreen,
            scaler: Some(GamescopeScaler::Fit),
            filter: Some(GamescopeFilter::Fsr),
            sharpness: Some(5),
            hdr: true,
            grab_keyboard: true,
            force_grab_cursor: true,
            extra_args: "--adaptive-sync --cursor '/home/me/my cursor.png'".into(),
        };
        assert_eq!(
            command(&settings, true).expect("building"),
            [
                "gamescope",
                "-w",
                "1280",
                "-h",
                "720",
                "-W",
                "2560",
                "-H",
                "1440",
                "-r",
                "144",
                "-f",
                "-S",
                "fit",
                "-F",
                "fsr",
                "--sharpness",
                "5",
                "--hdr-enabled",
                "-g",
                "--force-grab-cursor",
                "--mangoapp",
                "--adaptive-sync",
                "--cursor",
                "/home/me/my cursor.png",
                "--",
            ]
        );
    }

    #[test]
    fn broken_extra_arguments_stop_the_launch() {
        let settings = Gamescope {
            extra_args: "--cursor 'unclosed".into(),
            ..Gamescope::default()
        };
        assert!(command(&settings, false).is_err());
    }

    #[test]
    fn a_resolution_reads_the_way_people_type_it() {
        assert_eq!(parse_resolution("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_resolution(" 1280 × 720 "), Some((1280, 720)));
        assert_eq!(parse_resolution(""), Some((0, 0)));
        assert_eq!(parse_resolution("1920"), None);
        assert_eq!(parse_resolution("0x720"), None);
        assert_eq!(parse_resolution("wide x tall"), None);
        assert_eq!(show_resolution(1920, 1080), "1920x1080");
        assert_eq!(show_resolution(0, 0), "");
    }
}
