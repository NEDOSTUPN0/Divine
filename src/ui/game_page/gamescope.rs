//! The gamescope subpage of a game's settings.
//!
//! Only the options that matter for a game in a window on the desktop get a
//! row of their own. The rest of what gamescope takes (VR, a session of its
//! own on the bare display, debugging) goes through Extra Arguments.

use adw::prelude::*;

use crate::config::{GamescopeFilter, GamescopeScaler, GamescopeWindow};
use crate::gamescope::{parse_resolution, show_resolution};

use super::settings::Writer;

const SCALERS: [(&str, Option<GamescopeScaler>); 6] = [
    ("Default", None),
    ("Auto", Some(GamescopeScaler::Auto)),
    ("Integer", Some(GamescopeScaler::Integer)),
    ("Fit", Some(GamescopeScaler::Fit)),
    ("Fill", Some(GamescopeScaler::Fill)),
    ("Stretch", Some(GamescopeScaler::Stretch)),
];

const FILTERS: [(&str, Option<GamescopeFilter>); 7] = [
    ("Default", None),
    ("Linear", Some(GamescopeFilter::Linear)),
    ("Nearest", Some(GamescopeFilter::Nearest)),
    ("AMD FSR", Some(GamescopeFilter::Fsr)),
    ("NVIDIA NIS", Some(GamescopeFilter::Nis)),
    ("Pixel", Some(GamescopeFilter::Pixel)),
    ("Snapdragon GSR", Some(GamescopeFilter::Sgsr)),
];

const WINDOWS: [(&str, GamescopeWindow); 3] = [
    ("Windowed", GamescopeWindow::Windowed),
    ("Borderless", GamescopeWindow::Borderless),
    ("Fullscreen", GamescopeWindow::Fullscreen),
];

/// The row in the Launching group that leads to the subpage. `closed` runs
/// each time the subpage is left.
pub(super) fn gamescope_row(
    dialog: &adw::PreferencesDialog,
    write: &Writer,
    closed: impl Fn() + 'static,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title("Gamescope")
        .subtitle("Runs the game in a compositor of its own, at a resolution of its own")
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

    let dialog = dialog.downgrade();
    let write = write.clone();
    let closed = std::rc::Rc::new(closed);
    row.connect_activated(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            let page = page(&write);
            let closed = closed.clone();
            page.connect_hidden(move |_| closed());
            dialog.push_subpage(&page);
        }
    });
    crate::ui::needs_tool(&row, crate::gamescope::BINARY);
    row
}

/// Built when it is opened, so it always starts from what is saved.
fn page(write: &Writer) -> adw::NavigationPage {
    let saved = write.settings().gamescope;

    let enabled = write.switch(
        "Use Gamescope",
        "Starts the game inside a gamescope window",
        saved.enabled,
        |settings, on| settings.gamescope.enabled = on,
    );
    let main = adw::PreferencesGroup::builder()
        .description(
            "With MangoHud on, gamescope draws the overlay itself through mangoapp \
             where that is installed, as gamescope recommends.",
        )
        .build();
    main.add(&enabled);

    let game_resolution = resolution_row(
        write,
        "Game Resolution",
        "What the game renders at, e.g. 1280x720. Empty leaves it to gamescope.",
        (saved.game_width, saved.game_height),
        |settings, (width, height)| {
            settings.gamescope.game_width = width;
            settings.gamescope.game_height = height;
        },
    );
    let output_resolution = resolution_row(
        write,
        "Window Resolution",
        "The window gamescope opens, which the game is scaled up to fill. Empty \
         leaves it to gamescope.",
        (saved.output_width, saved.output_height),
        |settings, (width, height)| {
            settings.gamescope.output_width = width;
            settings.gamescope.output_height = height;
        },
    );
    let refresh_rate = adw::SpinRow::builder()
        .title("Refresh Rate")
        .subtitle("The rate the game runs at, in frames per second. 0 leaves it to gamescope.")
        .adjustment(&gtk::Adjustment::new(
            f64::from(saved.refresh_rate),
            0.0,
            1000.0,
            1.0,
            10.0,
            0.0,
        ))
        .build();
    {
        let write = write.clone();
        refresh_rate.connect_value_notify(move |row| {
            let rate = row.value().round() as u32;
            write.set(|settings| settings.gamescope.refresh_rate = rate);
        });
    }
    let window = choice_row(
        write,
        "Window",
        &WINDOWS,
        saved.window,
        |settings, window| settings.gamescope.window = window,
    );

    let display = adw::PreferencesGroup::builder().title("Display").build();
    display.add(&game_resolution);
    display.add(&output_resolution);
    display.add(&refresh_rate);
    display.add(&window);
    display.add(&write.switch(
        "HDR",
        "Has gamescope output HDR and DXVK render it. Needs a display that \
         supports it. Wine Wayland is not needed here.",
        saved.hdr,
        |settings, on| settings.gamescope.hdr = on,
    ));

    let scaler = choice_row(
        write,
        "Scaler",
        &SCALERS,
        saved.scaler,
        |settings, scaler| settings.gamescope.scaler = scaler,
    );
    scaler.set_subtitle("How the frame is fitted into the window");
    let filter = choice_row(
        write,
        "Filter",
        &FILTERS,
        saved.filter,
        |settings, filter| settings.gamescope.filter = filter,
    );
    filter.set_subtitle("How the frame is resampled on the way up");
    let sharpness = sharpness_row(write, saved.sharpness);

    let upscaling = adw::PreferencesGroup::builder().title("Upscaling").build();
    upscaling.add(&scaler);
    upscaling.add(&filter);
    upscaling.add(&sharpness);

    let grab_keyboard = write.switch(
        "Grab Keyboard",
        "Sends every key to the game while its window has focus",
        saved.grab_keyboard,
        |settings, on| settings.gamescope.grab_keyboard = on,
    );
    let force_grab_cursor = write.switch(
        "Always Relative Mouse",
        "Keeps the mouse in relative mode instead of switching whenever the \
         cursor shows or hides. Helps a camera that jumps or drifts.",
        saved.force_grab_cursor,
        |settings, on| settings.gamescope.force_grab_cursor = on,
    );
    let input = adw::PreferencesGroup::builder().title("Input").build();
    input.add(&grab_keyboard);
    input.add(&force_grab_cursor);

    let extra_args = write.entry(
        "Extra Arguments",
        &saved.extra_args,
        "Any other gamescope option, e.g. --hdr-enabled or --adaptive-sync. \
         gamescope --help lists them all.",
        |settings, value| settings.gamescope.extra_args = value,
    );
    // Marked while typing, like the launch options, which split the same way.
    extra_args.connect_changed(|row| {
        mark(row, crate::launch_options::split(&row.text()).is_ok());
    });
    let advanced = adw::PreferencesGroup::builder().title("Advanced").build();
    advanced.add(&extra_args);

    // Nothing below means anything while gamescope is off, but each keeps its
    // value for when it is back on.
    for group in [&display, &upscaling, &input, &advanced] {
        enabled
            .bind_property("active", group, "sensitive")
            .sync_create()
            .build();
    }

    let content = adw::PreferencesPage::new();
    for group in [&main, &display, &upscaling, &input, &advanced] {
        content.add(group);
    }
    let view = adw::ToolbarView::builder().content(&content).build();
    view.add_top_bar(&adw::HeaderBar::new());
    adw::NavigationPage::builder()
        .title("Gamescope")
        .child(&view)
        .build()
}

/// A `WIDTHxHEIGHT` field. Written on apply, and only when it reads as one.
fn resolution_row(
    write: &Writer,
    title: &str,
    tooltip: &str,
    (width, height): (u32, u32),
    set: impl Fn(&mut crate::config::GameSettings, (u32, u32)) + 'static,
) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .text(show_resolution(width, height))
        .show_apply_button(true)
        .build();
    row.set_tooltip_text(Some(tooltip));
    row.connect_changed(|row| mark(row, parse_resolution(&row.text()).is_some()));

    let write = write.clone();
    row.connect_apply(move |row| {
        if let Some(size) = parse_resolution(&row.text()) {
            write.set(|settings| set(settings, size));
        }
    });
    row
}

/// One of a fixed list of values, by its label.
fn choice_row<T: Copy + PartialEq + 'static>(
    write: &Writer,
    title: &str,
    choices: &'static [(&'static str, T)],
    current: T,
    set: impl Fn(&mut crate::config::GameSettings, T) + 'static,
) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(label, _)| *label).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .selected(
            choices
                .iter()
                .position(|(_, value)| *value == current)
                .unwrap_or(0) as u32,
        )
        .build();
    let write = write.clone();
    row.connect_selected_notify(move |row| {
        if let Some((_, value)) = choices.get(row.selected() as usize) {
            write.set(|settings| set(settings, *value));
        }
    });
    row
}

/// gamescope's own scale runs from 0, the sharpest, to 20. A plain spin row
/// could not say "not set", and that is where every game starts.
fn sharpness_row(write: &Writer, current: Option<u8>) -> adw::ComboRow {
    let mut labels = vec!["Default".to_string()];
    labels.extend((0..=20u8).map(|level| match level {
        0 => "0, sharpest".to_string(),
        20 => "20, softest".to_string(),
        level => level.to_string(),
    }));
    let row = adw::ComboRow::builder()
        .title("Sharpness")
        .subtitle("How hard the upscaler sharpens")
        .model(&gtk::StringList::new(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .selected(current.map_or(0, |level| u32::from(level) + 1))
        .build();
    let write = write.clone();
    row.connect_selected_notify(move |row| {
        let level = row.selected().checked_sub(1).map(|level| level as u8);
        write.set(|settings| settings.gamescope.sharpness = level);
    });
    row
}

fn mark(row: &adw::EntryRow, valid: bool) {
    if valid {
        row.remove_css_class("error");
    } else {
        row.add_css_class("error");
    }
}
