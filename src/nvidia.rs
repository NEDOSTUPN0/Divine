//! What Divine can tell NVIDIA's driver about a game: DLSS overrides and
//! Smooth Motion. Only means anything with NVIDIA's own driver loaded.
//!
//! DLSS is driven the way the NVIDIA app drives it on Windows, through the
//! driver profile. Under Proton that profile is dxvk-nvapi's to answer, and it
//! answers from `DXVK_NVAPI_DRS_*` variables, one per setting. Those win over
//! the `DXVK_NVAPI_DRS_SETTINGS` line GE-Proton and CachyOS write for their
//! own DLSS upgrade, so the two never have to be merged. The setting and value
//! names are dxvk-nvapi's (src/util/util_drs.h, v0.9.3 as GE-Proton 11 ships
//! it), which takes them from NVIDIA's NvApiDriverSettings.h.
//!
//! The upgrade itself is Proton's; see src/upscalers.rs. Recent GE-Proton
//! (11-12 has it, 11-7 does not) upgrades Streamline the same way, the SDK
//! many games reach DLSS through, whose old copies ignore the driver's frame
//! generation overrides.

use std::fs;

use crate::config::{Dlss, DlssMode, DlssPreset};
use crate::upscalers::wanted;

/// Where the kernel module says which driver is loaded. Absent with nouveau,
/// which has no DLSS and no Smooth Motion.
const DRIVER_VERSION_FILE: &str = "/proc/driver/nvidia/version";

/// The first driver with Smooth Motion on Linux, 575.51.02.
const SMOOTH_MOTION_SINCE: u32 = 575;

/// The presets each feature knows, newest model first. From the DLSS SDK
/// headers (nvsdk_ngx_defs.h, nvsdk_ngx_defs_dlssd.h, v310.9.16): Super
/// Resolution dropped A to D, E and F are deprecated, and the letters left out
/// fall back to the default. Frame generation's are those the NVIDIA app
/// offers, C still in its beta, and DLSS NR's those the driver profile lists.
pub const SR_PRESETS: &[DlssPreset] = &[
    DlssPreset::Latest,
    DlssPreset::M,
    DlssPreset::L,
    DlssPreset::K,
    DlssPreset::J,
    DlssPreset::F,
    DlssPreset::E,
];
pub const RR_PRESETS: &[DlssPreset] = &[
    DlssPreset::Latest,
    DlssPreset::F,
    DlssPreset::E,
    DlssPreset::D,
];
pub const FG_PRESETS: &[DlssPreset] = &[
    DlssPreset::Latest,
    DlssPreset::C,
    DlssPreset::B,
    DlssPreset::A,
];
pub const NR_PRESETS: &[DlssPreset] = &[
    DlssPreset::Latest,
    DlssPreset::D,
    DlssPreset::C,
    DlssPreset::B,
    DlssPreset::A,
];

/// The range the driver accepts for a custom render scale, in percent.
pub const SCALE_RANGE: std::ops::RangeInclusive<u8> = 33..=100;
/// Frames shown per frame rendered, as far as the driver goes today.
pub const MULTIPLIER_RANGE: std::ops::RangeInclusive<u8> = 2..=6;

/// The major version of the NVIDIA driver in use, if it is NVIDIA's.
pub fn driver_major() -> Option<u32> {
    parse_driver_major(&fs::read_to_string(DRIVER_VERSION_FILE).ok()?)
}

/// Reads `NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  615.78.08
/// Release Build ...`, which the proprietary module words differently but
/// numbers the same way.
fn parse_driver_major(text: &str) -> Option<u32> {
    let line = text
        .lines()
        .find(|line| line.starts_with("NVRM version:"))?;
    line.split_whitespace()
        .find(|word| word.split('.').count() >= 2 && word.split('.').all(is_number))?
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn is_number(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
}

pub fn smooth_motion_supported(driver_major: u32) -> bool {
    driver_major >= SMOOTH_MOTION_SINCE
}

/// The environment that carries a game's DLSS settings and Smooth Motion.
pub fn env(dlss: &Dlss, smooth_motion: bool) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut drs = |name: &str, value: String| {
        env.push((format!("DXVK_NVAPI_DRS_{name}"), value));
    };

    // Each feature's override is switched on next to anything changed about
    // it, which is how Proton pairs them for its own upgrade.
    if dlss.sr_preset.is_some() || dlss.sr_mode.is_some() {
        drs("NGX_DLSS_SR_OVERRIDE", "on".into());
    }
    if let Some(preset) = dlss.sr_preset {
        drs(
            "NGX_DLSS_SR_OVERRIDE_RENDER_PRESET_SELECTION",
            preset_value(preset),
        );
    }
    if let Some(mode) = dlss.sr_mode {
        drs("NGX_DLSS_SR_MODE", mode_value(mode).into());
        if mode == DlssMode::Custom {
            drs("NGX_DLSS_SR_OVERRIDE_SCALING_RATIO", scale(dlss.sr_scale));
        }
    }

    if dlss.rr_preset.is_some() || dlss.rr_mode.is_some() {
        drs("NGX_DLSS_RR_OVERRIDE", "on".into());
    }
    if let Some(preset) = dlss.rr_preset {
        drs(
            "NGX_DLSS_RR_OVERRIDE_RENDER_PRESET_SELECTION",
            preset_value(preset),
        );
    }
    if let Some(mode) = dlss.rr_mode {
        drs("NGX_DLSS_RR_MODE", mode_value(mode).into());
        if mode == DlssMode::Custom {
            drs("NGX_DLSS_RR_OVERRIDE_SCALING_RATIO", scale(dlss.rr_scale));
        }
    }

    let multiplier = (!dlss.fg_dynamic && dlss.fg_multiplier > 0).then_some(dlss.fg_multiplier);
    if dlss.fg_preset.is_some() || multiplier.is_some() || dlss.fg_dynamic {
        drs("NGX_DLSS_FG_OVERRIDE", "on".into());
    }
    if let Some(preset) = dlss.fg_preset {
        drs(
            "NGX_DLSS_FG_OVERRIDE_RENDER_PRESET_SELECTION",
            preset_value(preset),
        );
    }
    // The driver counts generated frames, not frames shown: 4x is 3.
    if let Some(multiplier) = multiplier {
        drs("NGX_DLSSG_MULTI_FRAME_COUNT", generated(multiplier));
    }
    if dlss.fg_dynamic {
        drs("NGX_DLSSG_MODE", "dynamic".into());
        drs(
            "NGX_DLSSG_DYNAMIC_TARGET_FRAME_RATE",
            match dlss.fg_target_fps {
                0 => "auto".into(),
                fps => fps.to_string(),
            },
        );
        if dlss.fg_dynamic_max > 0 {
            drs(
                "NGX_DLSSG_DYNAMIC_MULTI_FRAME_COUNT_MAX",
                generated(dlss.fg_dynamic_max),
            );
        }
    }

    if let Some(preset) = dlss.nr_preset {
        drs("NGX_DLSS_NR_OVERRIDE", "on".into());
        drs(
            "NGX_DLSS_NR_OVERRIDE_RENDER_PRESET_SELECTION",
            preset_value(preset),
        );
    }

    if dlss.upgrade {
        env.push(("PROTON_DLSS_UPGRADE".into(), wanted(&dlss.version)));
    }
    // umu-protonfixes only upgrades Streamline alongside DLSS, which GE-Proton
    // follows from 11-13 on, and the Streamline it brings ships no
    // nvngx_dlss.dll of its own to stand in for the game's.
    if dlss.upgrade && dlss.streamline {
        env.push((
            "PROTON_STREAMLINE_UPGRADE".into(),
            wanted(&dlss.streamline_version),
        ));
    }
    // Set directly rather than through PROTON_DLSS_INDICATOR, which only
    // GE-Proton and CachyOS read. dxvk-nvapi writes it into the prefix's
    // registry, so it outlives the launch; those two builds write it back to
    // off on every launch without it, Valve's does not.
    if dlss.indicator {
        env.push((
            "DXVK_NVAPI_SET_NGX_DEBUG_OPTIONS".into(),
            "DLSSIndicator=1024,DLSSGIndicator=2".into(),
        ));
    }

    if smooth_motion {
        env.push(("NVPRESENT_ENABLE_SMOOTH_MOTION".into(), "1".into()));
    }
    env
}

fn preset_value(preset: DlssPreset) -> String {
    let letter = match preset {
        DlssPreset::Latest => "latest",
        DlssPreset::A => "a",
        DlssPreset::B => "b",
        DlssPreset::C => "c",
        DlssPreset::D => "d",
        DlssPreset::E => "e",
        DlssPreset::F => "f",
        DlssPreset::J => "j",
        DlssPreset::K => "k",
        DlssPreset::L => "l",
        DlssPreset::M => "m",
    };
    format!("render_preset_{letter}")
}

fn mode_value(mode: DlssMode) -> &'static str {
    match mode {
        DlssMode::Dlaa => "dlaa",
        DlssMode::Quality => "quality",
        DlssMode::Balanced => "balanced",
        DlssMode::Performance => "performance",
        DlssMode::UltraPerformance => "ultra_performance",
        DlssMode::Custom => "custom",
    }
}

/// A scale never saved, or out of range, becomes the nearest the driver takes.
fn scale(percent: u8) -> String {
    percent
        .clamp(*SCALE_RANGE.start(), *SCALE_RANGE.end())
        .to_string()
}

fn generated(multiplier: u8) -> String {
    (multiplier.clamp(*MULTIPLIER_RANGE.start(), *MULTIPLIER_RANGE.end()) - 1).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn an_untouched_game_gets_no_environment() {
        assert!(env(&Dlss::default(), false).is_empty());
    }

    #[test]
    fn a_preset_turns_its_override_on() {
        let dlss = Dlss {
            sr_preset: Some(DlssPreset::K),
            ..Dlss::default()
        };
        let env = env(&dlss, false);
        assert_eq!(
            value(&env, "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE"),
            Some("on")
        );
        assert_eq!(
            value(
                &env,
                "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE_RENDER_PRESET_SELECTION"
            ),
            Some("render_preset_k")
        );
        assert_eq!(value(&env, "DXVK_NVAPI_DRS_NGX_DLSS_RR_OVERRIDE"), None);
    }

    #[test]
    fn a_custom_scale_only_travels_with_the_custom_mode() {
        let mut dlss = Dlss {
            sr_mode: Some(DlssMode::Quality),
            sr_scale: 75,
            ..Dlss::default()
        };
        let quality = env(&dlss, false);
        assert_eq!(
            value(&quality, "DXVK_NVAPI_DRS_NGX_DLSS_SR_MODE"),
            Some("quality")
        );
        assert_eq!(
            value(
                &quality,
                "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE_SCALING_RATIO"
            ),
            None
        );

        dlss.sr_mode = Some(DlssMode::Custom);
        dlss.sr_scale = 0;
        let custom = env(&dlss, false);
        assert_eq!(
            value(&custom, "DXVK_NVAPI_DRS_NGX_DLSS_SR_MODE"),
            Some("custom")
        );
        // Never saved: the nearest the driver accepts, not a zero it rejects.
        assert_eq!(
            value(&custom, "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE_SCALING_RATIO"),
            Some("33")
        );
    }

    #[test]
    fn a_multiplier_is_written_as_the_frames_generated() {
        let dlss = Dlss {
            fg_multiplier: 4,
            ..Dlss::default()
        };
        let env = env(&dlss, false);
        assert_eq!(
            value(&env, "DXVK_NVAPI_DRS_NGX_DLSSG_MULTI_FRAME_COUNT"),
            Some("3")
        );
        assert_eq!(
            value(&env, "DXVK_NVAPI_DRS_NGX_DLSS_FG_OVERRIDE"),
            Some("on")
        );
    }

    #[test]
    fn dynamic_frame_generation_replaces_a_fixed_multiplier() {
        let dlss = Dlss {
            fg_multiplier: 4,
            fg_dynamic: true,
            fg_dynamic_max: 6,
            ..Dlss::default()
        };
        let env = env(&dlss, false);
        assert_eq!(
            value(&env, "DXVK_NVAPI_DRS_NGX_DLSSG_MULTI_FRAME_COUNT"),
            None
        );
        assert_eq!(
            value(&env, "DXVK_NVAPI_DRS_NGX_DLSSG_MODE"),
            Some("dynamic")
        );
        assert_eq!(
            value(&env, "DXVK_NVAPI_DRS_NGX_DLSSG_DYNAMIC_TARGET_FRAME_RATE"),
            Some("auto")
        );
        assert_eq!(
            value(
                &env,
                "DXVK_NVAPI_DRS_NGX_DLSSG_DYNAMIC_MULTI_FRAME_COUNT_MAX"
            ),
            Some("5")
        );
    }

    #[test]
    fn an_upgrade_without_a_version_takes_the_newest() {
        let mut dlss = Dlss {
            upgrade: true,
            ..Dlss::default()
        };
        assert_eq!(value(&env(&dlss, false), "PROTON_DLSS_UPGRADE"), Some("1"));
        dlss.version = "310.5.0.0".into();
        assert_eq!(
            value(&env(&dlss, false), "PROTON_DLSS_UPGRADE"),
            Some("310.5.0.0")
        );
    }

    #[test]
    fn streamline_is_only_upgraded_alongside_dlss() {
        let mut dlss = Dlss {
            streamline: true,
            streamline_version: "2.14.1".into(),
            ..Dlss::default()
        };
        assert_eq!(value(&env(&dlss, false), "PROTON_STREAMLINE_UPGRADE"), None);
        dlss.upgrade = true;
        assert_eq!(
            value(&env(&dlss, false), "PROTON_STREAMLINE_UPGRADE"),
            Some("2.14.1")
        );
    }

    #[test]
    fn a_version_picked_but_not_upgraded_to_stays_home() {
        let dlss = Dlss {
            version: "310.5.0.0".into(),
            ..Dlss::default()
        };
        assert!(env(&dlss, false).is_empty());
    }

    #[test]
    fn smooth_motion_is_one_variable() {
        assert_eq!(
            env(&Dlss::default(), true),
            vec![(
                "NVPRESENT_ENABLE_SMOOTH_MOTION".to_string(),
                "1".to_string()
            )]
        );
    }

    #[test]
    fn the_driver_version_is_read_from_either_module() {
        assert_eq!(
            parse_driver_major(
                "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  615.78.08  \
                 Release Build  (nixbld@)  \nGCC version:  gcc version 16.2.0 (GCC) \n"
            ),
            Some(615)
        );
        assert_eq!(
            parse_driver_major(
                "NVRM version: NVIDIA UNIX x86_64 Kernel Module  575.64  Tue Jun 10 2025\n"
            ),
            Some(575)
        );
        assert_eq!(parse_driver_major(""), None);
    }
}
