//! The Upscalers subpage of a game's settings: Smooth Motion and XeSS on the
//! page itself, DLSS and FSR on subpages of their own.
//!
//! Everything is shown on every machine, so a setting never vanishes without
//! a word; what the cards present cannot run is locked, and says why. What
//! each row turns into is src/upscalers.rs and src/nvidia.rs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::config::{DlssMode, DlssPreset, GameSettings};
use crate::gpu::{self, NvidiaCard};
use crate::nvidia::{
    self, FG_PRESETS, MULTIPLIER_RANGE, NR_PRESETS, RR_PRESETS, SCALE_RANGE, SR_PRESETS,
};
use crate::ui::spawn_bg;

use super::settings::Writer;

const MODES: [(&str, Option<DlssMode>); 7] = [
    ("Game's Choice", None),
    ("DLAA", Some(DlssMode::Dlaa)),
    ("Quality", Some(DlssMode::Quality)),
    ("Balanced", Some(DlssMode::Balanced)),
    ("Performance", Some(DlssMode::Performance)),
    ("Ultra Performance", Some(DlssMode::UltraPerformance)),
    ("Custom", Some(DlssMode::Custom)),
];

const NEEDS_RTX: &str = "Needs an NVIDIA RTX card";
const NEEDS_RTX_40: &str = "Needs an NVIDIA RTX 40 series card or newer";

/// The row on the Graphics tab that leads to the subpage.
pub(super) fn upscalers_row(dialog: &adw::PreferencesDialog, write: &Writer) -> adw::ActionRow {
    let row = link_row("Upscalers", "DLSS, FSR, XeSS and Smooth Motion");
    let dialog = dialog.downgrade();
    let write = write.clone();
    row.connect_activated(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.push_subpage(&page(&dialog, &write));
        }
    });
    row
}

fn link_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

/// A row leading to a further subpage, built when it is opened.
fn subpage_row(
    dialog: &adw::PreferencesDialog,
    write: &Writer,
    title: &str,
    subtitle: &str,
    build: fn(&Writer) -> adw::NavigationPage,
) -> adw::ActionRow {
    let row = link_row(title, subtitle);
    let dialog = dialog.downgrade();
    let write = write.clone();
    row.connect_activated(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.push_subpage(&build(&write));
        }
    });
    row
}

fn navigation_page(title: &str, groups: &[&adw::PreferencesGroup]) -> adw::NavigationPage {
    let content = adw::PreferencesPage::new();
    for group in groups {
        content.add(*group);
    }
    let view = adw::ToolbarView::builder().content(&content).build();
    view.add_top_bar(&adw::HeaderBar::new());
    adw::NavigationPage::builder()
        .title(title)
        .child(&view)
        .build()
}

/// Built when it is opened, so it always starts from what is saved.
fn page(dialog: &adw::PreferencesDialog, write: &Writer) -> adw::NavigationPage {
    let saved = write.settings();
    let card = gpu::detected().nvidia.as_ref();

    let motion = smooth_motion_group(
        write,
        &saved,
        card.is_some_and(NvidiaCard::frame_generation),
    );

    let dlss = subpage_row(
        dialog,
        write,
        "DLSS",
        "Presets, modes, frame generation and newer libraries",
        dlss_page,
    );
    if !card.is_some_and(NvidiaCard::dlss) {
        dlss.set_sensitive(false);
        dlss.set_subtitle(NEEDS_RTX);
    }
    let fsr = subpage_row(
        dialog,
        write,
        "FSR",
        "FSR 4 and newer FidelityFX libraries",
        fsr_page,
    );
    let (xess, fill_xess) = upgrade_row(
        write,
        "Upgrade XeSS",
        "Loads newer XeSS libraries in place of the game's, frame generation \
         included. Works on any card, fastest on Intel's own.",
        saved.xess.upgrade,
        |s, on| s.xess.upgrade = on,
        ("XeSS Version", "Newest", &saved.xess.version, |s, v| {
            s.xess.version = v
        }),
    );
    spawn_bg(crate::upscalers::versions, move |listed| {
        if let Some(listed) = listed {
            fill_xess(listed.xess);
        }
    });

    let upscalers = adw::PreferencesGroup::builder()
        .title("Upscalers")
        .description(UPGRADES_NEED)
        .build();
    upscalers.add(&dlss);
    upscalers.add(&fsr);
    upscalers.add(&xess);
    navigation_page("Upscalers", &[&motion, &upscalers])
}

/// Newer libraries come from Proton, which loads them in place of the game's
/// and leaves its files alone.
const UPGRADES_NEED: &str = "Upgrades need GE-Proton or CachyOS, which download the newer \
     libraries into the prefix on the next launch. The game's own files stay as they are.";

/// Locks a group the cards present cannot use, keeping its values for a
/// machine that can.
fn unsupported(group: &adw::PreferencesGroup, why: &str) {
    group.set_sensitive(false);
    group.set_description(Some(why));
}

fn smooth_motion_group(
    write: &Writer,
    saved: &GameSettings,
    card_supports: bool,
) -> adw::PreferencesGroup {
    let row = write.switch(
        "Smooth Motion",
        "Makes up a frame between every two the game renders, for games without \
         DLSS Frame Generation. Do not combine it with Frame Generation: that costs \
         performance and adds artifacts.",
        saved.smooth_motion,
        |settings, on| settings.smooth_motion = on,
    );
    let group = adw::PreferencesGroup::new();
    group.add(&row);

    let driver = nvidia::driver_major();
    if !card_supports {
        row.set_sensitive(false);
        row.set_subtitle(NEEDS_RTX_40);
    } else if let Some(driver) = driver.filter(|d| !nvidia::smooth_motion_supported(*d)) {
        row.set_sensitive(false);
        row.set_subtitle(&format!(
            "Needs NVIDIA driver 575 or newer, and this one is {driver}"
        ));
    }
    group
}

/// What a version row needs: its title, what Proton's own pick is called,
/// what is saved, and where it is kept.
type VersionSpec<'a> = (
    &'a str,
    &'static str,
    &'a str,
    fn(&mut GameSettings, String),
);

/// An upgrade with its switch in the header and the version inside, the
/// way GNOME's settings pair a switch with what it opens up. The returned
/// closure fills in the versions once Proton's list has answered.
fn upgrade_row(
    write: &Writer,
    title: &str,
    subtitle: &str,
    on: bool,
    set: fn(&mut GameSettings, bool),
    (version_title, default, saved, set_version): VersionSpec,
) -> (adw::ExpanderRow, impl FnOnce(Vec<String>)) {
    let row = adw::ExpanderRow::builder()
        .title(title)
        .subtitle(subtitle)
        .show_enable_switch(true)
        .enable_expansion(on)
        .build();
    {
        let write = write.clone();
        row.connect_enable_expansion_notify(move |row| {
            let on = row.enables_expansion();
            write.set(|settings| set(settings, on));
        });
    }
    let (version, fill) = version_row(write, version_title, default, saved, set_version);
    row.add_row(&version);
    (row, fill)
}

fn dlss_page(write: &Writer) -> adw::NavigationPage {
    let saved = write.settings();
    let card = gpu::detected().nvidia.as_ref();
    let dlss = &saved.dlss;

    let (upgrade, fill_dlss) = upgrade_row(
        write,
        "Upgrade DLSS",
        "Loads newer DLSS libraries in place of the ones the game shipped with",
        dlss.upgrade,
        |s, on| s.dlss.upgrade = on,
        ("Version", "Newest", &dlss.version, |s, v| {
            s.dlss.version = v
        }),
    );
    let (streamline, fill_streamline) = upgrade_row(
        write,
        "Upgrade Streamline",
        "Loads a newer Streamline, the NVIDIA SDK many games reach DLSS through. \
         An old one ignores the frame generation overrides. Goes with Upgrade DLSS, \
         and needs a recent GE-Proton: 11-12 has it.",
        dlss.streamline,
        |s, on| s.dlss.streamline = on,
        ("Version", "Newest", &dlss.streamline_version, |s, v| {
            s.dlss.streamline_version = v
        }),
    );
    // Locked, the switch keeps its position for when DLSS is upgraded again.
    upgrade
        .bind_property("enable-expansion", &streamline, "sensitive")
        .sync_create()
        .build();
    spawn_bg(crate::upscalers::versions, move |listed| {
        if let Some(listed) = listed {
            fill_dlss(listed.dlss);
            fill_streamline(listed.streamline);
        }
    });
    let indicator = write.switch(
        "Indicator",
        "Shows the DLSS version, preset and mode in a corner of the game",
        dlss.indicator,
        |settings, on| settings.dlss.indicator = on,
    );
    let general = adw::PreferencesGroup::builder()
        .description(
            "Overrides what the game asks DLSS for, as the NVIDIA app does on \
             Windows. Only games with DLSS are affected, and DLSS still has to be \
             switched on in the game's own settings.",
        )
        .build();
    general.add(&upgrade);
    general.add(&streamline);
    general.add(&indicator);

    let super_resolution = feature_group(
        write,
        "Super Resolution",
        SR_PRESETS,
        "Preset K suits DLAA, Quality and Balanced, M Performance and L Ultra Performance.",
        (dlss.sr_preset, dlss.sr_mode, dlss.sr_scale),
        Setters {
            preset: |s, value| s.dlss.sr_preset = value,
            mode: |s, value| s.dlss.sr_mode = value,
            scale: |s, value| s.dlss.sr_scale = value,
        },
    );
    let ray_reconstruction = feature_group(
        write,
        "Ray Reconstruction",
        RR_PRESETS,
        "Presets D and E are transformer models, E the newer.",
        (dlss.rr_preset, dlss.rr_mode, dlss.rr_scale),
        Setters {
            preset: |s, value| s.dlss.rr_preset = value,
            mode: |s, value| s.dlss.rr_mode = value,
            scale: |s, value| s.dlss.rr_scale = value,
        },
    );
    let frame_generation = frame_generation_group(
        write,
        &saved,
        card.is_some_and(NvidiaCard::multi_frame_generation),
    );
    if !card.is_some_and(NvidiaCard::frame_generation) {
        unsupported(&frame_generation, NEEDS_RTX_40);
    }
    let neural_rendering = adw::PreferencesGroup::builder()
        .title("DLSS NR")
        .description("For the games that ship nvngx_dlssnr.dll")
        .build();
    neural_rendering.add(&preset_row(
        write,
        NR_PRESETS,
        dlss.nr_preset,
        |s, value| s.dlss.nr_preset = value,
    ));

    navigation_page(
        "DLSS",
        &[
            &general,
            &super_resolution,
            &ray_reconstruction,
            &frame_generation,
            &neural_rendering,
        ],
    )
}

fn fsr_page(write: &Writer) -> adw::NavigationPage {
    let saved = write.settings();
    let fsr = &saved.fsr;

    let (fsr4, fill_fsr4) = upgrade_row(
        write,
        "Upgrade to FSR 4",
        "Runs FSR 4 in games that ship FSR 3.1, through AMD's driver library. \
         Radeon RX 7000 series and newer.",
        fsr.fsr4,
        |s, on| s.fsr.fsr4 = on,
        ("Version", "Newest", &fsr.fsr4_version, |s, v| {
            s.fsr.fsr4_version = v
        }),
    );
    fsr4.add_row(&write.switch(
        "ML Frame Generation",
        "AMD's machine learning frame generation in place of FSR's own",
        fsr.ml_frame_generation,
        |settings, on| settings.fsr.ml_frame_generation = on,
    ));
    fsr4.add_row(&write.switch(
        "Indicator",
        "Shows FSR's watermarks over the game, one for upscaling and one for \
         frame generation",
        fsr.indicator,
        |settings, on| settings.fsr.indicator = on,
    ));
    let fsr4_group = adw::PreferencesGroup::builder().title("FSR 4").build();
    fsr4_group.add(&fsr4);
    if !gpu::detected().amd {
        unsupported(&fsr4_group, "Needs an AMD Radeon card");
    }

    // Proton pins a version of its own here rather than taking the newest.
    let (sdk, fill_sdk) = upgrade_row(
        write,
        "Upgrade FidelityFX SDK",
        "Loads newer FSR 3.1 libraries in place of the game's",
        fsr.sdk,
        |s, on| s.fsr.sdk = on,
        ("Version", "Proton's Choice", &fsr.sdk_version, |s, v| {
            s.fsr.sdk_version = v
        }),
    );
    let sdk_group = adw::PreferencesGroup::builder()
        .title("FSR 3.1")
        .description("Works on any card")
        .build();
    sdk_group.add(&sdk);

    spawn_bg(crate::upscalers::versions, move |listed| {
        if let Some(listed) = listed {
            fill_fsr4(listed.fsr4);
            fill_sdk(listed.fidelityfx);
        }
    });
    let note = adw::PreferencesGroup::builder()
        .description(UPGRADES_NEED)
        .build();
    navigation_page("FSR", &[&note, &fsr4_group, &sdk_group])
}

/// Where each of a feature's three settings is kept.
struct Setters {
    preset: fn(&mut GameSettings, Option<DlssPreset>),
    mode: fn(&mut GameSettings, Option<DlssMode>),
    scale: fn(&mut GameSettings, u8),
}

/// Preset, mode and custom scale for one of the two upscaling features.
fn feature_group(
    write: &Writer,
    title: &str,
    presets: &[DlssPreset],
    description: &str,
    (preset, mode, scale): (Option<DlssPreset>, Option<DlssMode>, u8),
    set: Setters,
) -> adw::PreferencesGroup {
    let preset_row = preset_row(write, presets, preset, set.preset);

    let mode_row = choice_row(
        write,
        "Mode",
        MODES
            .iter()
            .map(|(label, mode)| (label.to_string(), *mode))
            .collect(),
        mode,
        set.mode,
    );
    mode_row.set_subtitle("How far below the output resolution the game renders");

    let scale_row = adw::SpinRow::builder()
        .title("Render Scale")
        .subtitle("Percent of the output resolution, for the Custom mode")
        .adjustment(&gtk::Adjustment::new(
            f64::from(if scale == 0 { 67 } else { scale }),
            f64::from(*SCALE_RANGE.start()),
            f64::from(*SCALE_RANGE.end()),
            1.0,
            5.0,
            0.0,
        ))
        .build();
    {
        let write = write.clone();
        scale_row.connect_value_notify(move |row| {
            let percent = row.value().round() as u8;
            write.set(|settings| (set.scale)(settings, percent));
        });
    }
    let custom = |row: &adw::ComboRow| row.selected() as usize == MODES.len() - 1;
    scale_row.set_sensitive(custom(&mode_row));
    // The scale shown is saved along with the Custom mode, so a game never
    // runs at a scale other than the one on screen.
    {
        let (scale_row, write) = (scale_row.clone(), write.clone());
        mode_row.connect_selected_notify(move |row| {
            scale_row.set_sensitive(custom(row));
            if custom(row) {
                let percent = scale_row.value().round() as u8;
                write.set(|settings| (set.scale)(settings, percent));
            }
        });
    }

    let group = adw::PreferencesGroup::builder()
        .title(title)
        .description(description)
        .build();
    group.add(&preset_row);
    group.add(&mode_row);
    group.add(&scale_row);
    group
}

/// Frames shown per frame rendered, as the multiplier row offers them.
#[derive(Clone, Copy, PartialEq)]
enum Multiplier {
    Game,
    Fixed(u8),
    Dynamic,
}

/// Without an RTX 50 the multiplier stops at 2× and has no Dynamic: those are
/// left out rather than offered and quietly dropped at launch.
fn frame_generation_group(
    write: &Writer,
    saved: &GameSettings,
    multi_frame: bool,
) -> adw::PreferencesGroup {
    let dlss = &saved.dlss;
    let preset = preset_row(write, FG_PRESETS, dlss.fg_preset, |s, value| {
        s.dlss.fg_preset = value
    });

    let mut multipliers = vec![("Game's Choice".to_string(), Multiplier::Game)];
    let highest = if multi_frame {
        *MULTIPLIER_RANGE.end()
    } else {
        *MULTIPLIER_RANGE.start()
    };
    multipliers.extend(
        (*MULTIPLIER_RANGE.start()..=highest).map(|n| (format!("{n}×"), Multiplier::Fixed(n))),
    );
    if multi_frame {
        multipliers.push(("Dynamic".to_string(), Multiplier::Dynamic));
    }
    let current = match (dlss.fg_dynamic, dlss.fg_multiplier) {
        (true, _) => Multiplier::Dynamic,
        (false, 0) => Multiplier::Game,
        (false, n) => Multiplier::Fixed(n),
    };
    let dynamic_index = multipliers
        .iter()
        .position(|(_, m)| *m == Multiplier::Dynamic);
    let multiplier = choice_row(
        write,
        "Multiplier",
        multipliers,
        current,
        |s, picked| match picked {
            Multiplier::Game => {
                s.dlss.fg_dynamic = false;
                s.dlss.fg_multiplier = 0;
            }
            Multiplier::Fixed(n) => {
                s.dlss.fg_dynamic = false;
                s.dlss.fg_multiplier = n;
            }
            Multiplier::Dynamic => s.dlss.fg_dynamic = true,
        },
    );
    multiplier.set_subtitle(if multi_frame {
        "Frames shown per frame rendered"
    } else {
        "Frames shown per frame rendered. More than 2× needs an RTX 50 series card."
    });

    let target = adw::SpinRow::builder()
        .title("Target Frame Rate")
        .subtitle("For the Dynamic multiplier. 0 aims for the display's refresh rate.")
        .adjustment(&gtk::Adjustment::new(
            f64::from(dlss.fg_target_fps),
            0.0,
            1000.0,
            1.0,
            10.0,
            0.0,
        ))
        .build();
    {
        let write = write.clone();
        target.connect_value_notify(move |row| {
            let fps = row.value().round() as u32;
            write.set(|settings| settings.dlss.fg_target_fps = fps);
        });
    }

    let mut ceilings = vec![("Driver's Choice".to_string(), 0)];
    ceilings.extend(MULTIPLIER_RANGE.map(|n| (format!("{n}×"), n)));
    let ceiling = choice_row(
        write,
        "Highest Multiplier",
        ceilings,
        dlss.fg_dynamic_max,
        |s, n| s.dlss.fg_dynamic_max = n,
    );
    ceiling.set_subtitle("How far the Dynamic multiplier may go");

    let dynamic = move |row: &adw::ComboRow| Some(row.selected() as usize) == dynamic_index;
    target.set_sensitive(dynamic(&multiplier));
    ceiling.set_sensitive(dynamic(&multiplier));
    {
        let (target, ceiling) = (target.clone(), ceiling.clone());
        multiplier.connect_selected_notify(move |row| {
            target.set_sensitive(dynamic(row));
            ceiling.set_sensitive(dynamic(row));
        });
    }

    let group = adw::PreferencesGroup::builder()
        .title("Frame Generation")
        .description(
            "For games with DLSS Frame Generation. Preset B is the newer model and \
             not every game takes it; C, still in beta, is faster at some cost to \
             quality.",
        )
        .build();
    group.add(&preset);
    group.add(&multiplier);
    if multi_frame {
        group.add(&target);
        group.add(&ceiling);
    }
    group
}

fn preset_row(
    write: &Writer,
    presets: &[DlssPreset],
    current: Option<DlssPreset>,
    set: fn(&mut GameSettings, Option<DlssPreset>),
) -> adw::ComboRow {
    let mut choices = vec![("Game's Choice".to_string(), None)];
    choices.extend(
        presets
            .iter()
            .map(|preset| (preset_label(*preset), Some(*preset))),
    );
    choice_row(write, "Preset", choices, current, set)
}

fn preset_label(preset: DlssPreset) -> String {
    let letter = match preset {
        DlssPreset::Latest => return "Latest".into(),
        DlssPreset::A => 'A',
        DlssPreset::B => 'B',
        DlssPreset::C => 'C',
        DlssPreset::D => 'D',
        DlssPreset::E => 'E',
        DlssPreset::F => 'F',
        DlssPreset::J => 'J',
        DlssPreset::K => 'K',
        DlssPreset::L => 'L',
        DlssPreset::M => 'M',
    };
    format!("Preset {letter}")
}

/// One of a list of values, by its label.
fn choice_row<T: Copy + PartialEq + 'static>(
    write: &Writer,
    title: &str,
    choices: Vec<(String, T)>,
    current: T,
    set: impl Fn(&mut GameSettings, T) + 'static,
) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(label, _)| label.as_str()).collect();
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

/// The version to upgrade to. Starts with Proton's own pick, labelled
/// `default`, and the one saved; the returned closure fills in the rest once
/// Proton's list has answered.
fn version_row(
    write: &Writer,
    title: &str,
    default: &'static str,
    saved: &str,
    set: fn(&mut GameSettings, String),
) -> (adw::ComboRow, impl FnOnce(Vec<String>)) {
    let label = move |version: &str| -> String {
        if version.is_empty() {
            default.to_string()
        } else {
            version.to_string()
        }
    };
    let versions = Rc::new(RefCell::new(vec![String::new()]));
    if !saved.is_empty() {
        versions.borrow_mut().push(saved.to_string());
    }
    let model = gtk::StringList::new(&[]);
    for version in versions.borrow().iter() {
        model.append(&label(version));
    }
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&model)
        .selected(u32::from(!saved.is_empty()))
        .build();

    // Filling the list in moves the selection, which is not a choice.
    let filling = Rc::new(Cell::new(false));
    {
        let (write, versions, filling) = (write.clone(), versions.clone(), filling.clone());
        row.connect_selected_notify(move |row| {
            if filling.get() {
                return;
            }
            if let Some(version) = versions.borrow().get(row.selected() as usize) {
                let version = version.clone();
                write.set(|settings| set(settings, version));
            }
        });
    }

    let target = row.downgrade();
    let saved = saved.to_string();
    let fill = move |listed: Vec<String>| {
        let Some(row) = target.upgrade() else {
            return;
        };
        let mut all = vec![String::new()];
        all.extend(listed);
        // A version the list no longer carries is still what Proton is told.
        if !saved.is_empty() && !all.contains(&saved) {
            all.push(saved.clone());
        }
        let selected = all.iter().position(|v| *v == saved).unwrap_or(0);
        let labels: Vec<String> = all.iter().map(|v| label(v)).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();

        filling.set(true);
        model.splice(0, model.n_items(), &labels);
        row.set_selected(selected as u32);
        filling.set(false);
        *versions.borrow_mut() = all;
    };
    (row, fill)
}
