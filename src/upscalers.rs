//! Upscalers and frame generation for one game: what is turned on, and only on
//! the cards that can run it.
//!
//! The upgrades are Proton's. GE-Proton and CachyOS download the libraries
//! listed in loathingkernel's proton-upscalers manifest into the prefix and
//! have Wine load them in place of the game's, leaving its files alone;
//! Valve's Proton has none of this and ignores the variables. DLSS's own
//! overrides and Smooth Motion are src/nvidia.rs.
//!
//! The FSR 4 pieces end up in Wine's amdxc64.dll, which reads FSR4_UPGRADE,
//! MLFG_UPGRADE and the FSR watermarks straight from the environment. Proton
//! sets FSR4_UPGRADE itself with the upgrade, but only ever sets MLFG_UPGRADE
//! to on for an environment key nobody sets (umu-protonfixes, upscalers.py),
//! so Divine sets that one directly; Proton only fills in what is missing.

use std::sync::Mutex;

use serde::Deserialize;

use crate::config::{Fsr, GameSettings, Xess};
use crate::gpu::Gpus;

/// The list GE-Proton and CachyOS pick upgrade versions from.
const MANIFEST: &str = "https://loathingkernel.github.io/proton-upscalers/manifest.json";

/// Everything a game's upscaler settings put in its environment, leaving out
/// what the cards present cannot run. `driver` is NVIDIA's major version.
pub fn env(settings: &GameSettings, gpus: &Gpus, driver: Option<u32>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(card) = &gpus.nvidia {
        let smooth_motion = settings.smooth_motion
            && card.frame_generation()
            && driver.is_some_and(crate::nvidia::smooth_motion_supported);
        // Frame generation implies DLSS, so a card without it has neither.
        if card.dlss() {
            let mut dlss = settings.dlss.clone();
            if !card.frame_generation() {
                dlss.fg_preset = None;
                dlss.fg_multiplier = 0;
                dlss.fg_dynamic = false;
            }
            if !card.multi_frame_generation() {
                if dlss.fg_multiplier > 2 {
                    dlss.fg_multiplier = 0;
                }
                dlss.fg_dynamic = false;
            }
            env.extend(crate::nvidia::env(&dlss, smooth_motion));
        }
    }
    env.extend(fsr_env(&settings.fsr, gpus.amd));
    env.extend(xess_env(&settings.xess));
    env
}

fn fsr_env(fsr: &Fsr, amd: bool) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if fsr.fsr4 && amd {
        env.push(("PROTON_FSR4_UPGRADE".into(), wanted(&fsr.fsr4_version)));
        if fsr.ml_frame_generation {
            env.push(("MLFG_UPGRADE".into(), "1".into()));
        }
        if fsr.indicator {
            env.push(("FSR_WATERMARK".into(), "1".into()));
            env.push(("FSR_FG_WATERMARK".into(), "1".into()));
        }
    }
    if fsr.sdk {
        // GE-Proton reads PROTON_FFX3_UPGRADE for the version alone: nothing
        // in its script turns the upgrade on from the environment except
        // Steam's own compatibility switches, which CachyOS reads as well.
        env.push(("STEAM_COMPAT_CONFIG".into(), "ffx3".into()));
        if !fsr.sdk_version.trim().is_empty() {
            env.push((
                "PROTON_FFX3_UPGRADE".into(),
                fsr.sdk_version.trim().to_string(),
            ));
        }
    }
    env
}

fn xess_env(xess: &Xess) -> Vec<(String, String)> {
    if xess.upgrade {
        vec![("PROTON_XESS_UPGRADE".into(), wanted(&xess.version))]
    } else {
        Vec::new()
    }
}

/// Proton reads anything but "1" as the version to fetch, and "1" as its own
/// pick.
pub fn wanted(version: &str) -> String {
    match version.trim() {
        "" => "1".to_string(),
        version => version.to_string(),
    }
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    dlss: Vec<ManifestEntry>,
    #[serde(default)]
    streamline: Vec<ManifestEntry>,
    #[serde(default)]
    fsr_40_drv: Vec<ManifestEntry>,
    #[serde(default)]
    fsr_31_dx12: Vec<ManifestEntry>,
    #[serde(default)]
    xess: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    version: String,
    is_dev_file: bool,
}

/// What Proton can upgrade a game to, each newest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Versions {
    pub dlss: Vec<String>,
    pub streamline: Vec<String>,
    pub fsr4: Vec<String>,
    /// The FidelityFX SDK, by its DirectX 12 library: Proton takes one version
    /// for that and the Vulkan one alike.
    pub fidelityfx: Vec<String>,
    pub xess: Vec<String>,
}

/// `None` when the list cannot be had, which leaves Proton's pick as the only
/// choice. Blocking. An answer is kept for the session, since every page of
/// upscaler settings asks; a failure is not, so going online helps.
pub fn versions() -> Option<Versions> {
    static SEEN: Mutex<Option<Versions>> = Mutex::new(None);
    if let Some(seen) = SEEN.lock().ok().and_then(|seen| seen.clone()) {
        return Some(seen);
    }
    let versions = parse_versions(&crate::http::ask(MANIFEST)?)?;
    if let Ok(mut seen) = SEEN.lock() {
        *seen = Some(versions.clone());
    }
    Some(versions)
}

fn parse_versions(manifest: &str) -> Option<Versions> {
    let manifest: Manifest = serde_json::from_str(manifest).ok()?;
    Some(Versions {
        dlss: newest_first(manifest.dlss),
        streamline: newest_first(manifest.streamline),
        fsr4: newest_first(manifest.fsr_40_drv),
        fidelityfx: newest_first(manifest.fsr_31_dx12),
        xess: newest_first(manifest.xess),
    })
}

/// Development builds are skipped, as Proton skips them when it picks one.
fn newest_first(entries: Vec<ManifestEntry>) -> Vec<String> {
    let mut versions: Vec<String> = Vec::new();
    for entry in entries.into_iter().rev() {
        if !entry.is_dev_file && !versions.contains(&entry.version) {
            versions.push(entry.version);
        }
    }
    versions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Dlss, DlssPreset};
    use crate::gpu::NvidiaCard;

    fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn nvidia(name: &str, compute: (u32, u32)) -> Gpus {
        Gpus {
            nvidia: Some(NvidiaCard {
                name: name.into(),
                compute: Some(compute),
            }),
            ..Gpus::default()
        }
    }

    fn amd() -> Gpus {
        Gpus {
            amd: true,
            ..Gpus::default()
        }
    }

    #[test]
    fn dlss_stays_home_on_a_card_without_it() {
        let settings = GameSettings {
            dlss: Dlss {
                sr_preset: Some(DlssPreset::K),
                ..Dlss::default()
            },
            ..GameSettings::default()
        };
        assert!(env(&settings, &amd(), None).is_empty());
        assert!(env(&settings, &nvidia("NVIDIA GeForce GTX 1660", (7, 5)), None).is_empty());
        assert!(!env(&settings, &nvidia("NVIDIA GeForce RTX 2060", (7, 5)), None).is_empty());
    }

    #[test]
    fn multi_frame_generation_stays_home_on_an_rtx_40() {
        let settings = GameSettings {
            dlss: Dlss {
                fg_multiplier: 4,
                ..Dlss::default()
            },
            ..GameSettings::default()
        };
        let rtx_40 = nvidia("NVIDIA GeForce RTX 4070", (8, 9));
        let rtx_50 = nvidia("NVIDIA GeForce RTX 5070", (12, 0));
        let key = "DXVK_NVAPI_DRS_NGX_DLSSG_MULTI_FRAME_COUNT";
        assert_eq!(value(&env(&settings, &rtx_40, None), key), None);
        assert_eq!(value(&env(&settings, &rtx_50, None), key), Some("3"));
    }

    #[test]
    fn smooth_motion_needs_an_rtx_40_and_a_driver_that_has_it() {
        let settings = GameSettings {
            smooth_motion: true,
            ..GameSettings::default()
        };
        let rtx_30 = nvidia("NVIDIA GeForce RTX 3080", (8, 6));
        let rtx_40 = nvidia("NVIDIA GeForce RTX 4070", (8, 9));
        assert!(env(&settings, &rtx_30, Some(615)).is_empty());
        assert!(env(&settings, &rtx_40, Some(570)).is_empty());
        assert_eq!(
            value(
                &env(&settings, &rtx_40, Some(615)),
                "NVPRESENT_ENABLE_SMOOTH_MOTION"
            ),
            Some("1")
        );
    }

    #[test]
    fn fsr_4_and_its_extras_only_travel_to_an_amd_card() {
        let settings = GameSettings {
            fsr: Fsr {
                fsr4: true,
                ml_frame_generation: true,
                indicator: true,
                ..Fsr::default()
            },
            ..GameSettings::default()
        };
        assert!(env(&settings, &nvidia("NVIDIA GeForce RTX 5070", (12, 0)), None).is_empty());
        let on_amd = env(&settings, &amd(), None);
        assert_eq!(value(&on_amd, "PROTON_FSR4_UPGRADE"), Some("1"));
        assert_eq!(value(&on_amd, "MLFG_UPGRADE"), Some("1"));
        assert_eq!(value(&on_amd, "FSR_WATERMARK"), Some("1"));
    }

    #[test]
    fn ml_frame_generation_does_not_come_without_fsr_4() {
        let settings = GameSettings {
            fsr: Fsr {
                ml_frame_generation: true,
                ..Fsr::default()
            },
            ..GameSettings::default()
        };
        assert!(env(&settings, &amd(), None).is_empty());
    }

    #[test]
    fn the_fidelityfx_sdk_upgrade_is_switched_on_through_compat_config() {
        let mut settings = GameSettings {
            fsr: Fsr {
                sdk: true,
                ..Fsr::default()
            },
            ..GameSettings::default()
        };
        let any = Gpus::default();
        let env_now = env(&settings, &any, None);
        assert_eq!(value(&env_now, "STEAM_COMPAT_CONFIG"), Some("ffx3"));
        assert_eq!(value(&env_now, "PROTON_FFX3_UPGRADE"), None);

        settings.fsr.sdk_version = "1.0.2.38022".into();
        assert_eq!(
            value(&env(&settings, &any, None), "PROTON_FFX3_UPGRADE"),
            Some("1.0.2.38022")
        );
    }

    #[test]
    fn xess_is_upgraded_on_any_card() {
        let settings = GameSettings {
            xess: Xess {
                upgrade: true,
                version: "2.0.2.53".into(),
            },
            ..GameSettings::default()
        };
        assert_eq!(
            value(
                &env(&settings, &Gpus::default(), None),
                "PROTON_XESS_UPGRADE"
            ),
            Some("2.0.2.53")
        );
    }

    #[test]
    fn versions_come_newest_first_without_development_builds() {
        // Trimmed from the live manifest.
        let manifest = r#"{"dlss": [
            {"version": "310.9.1.0", "is_dev_file": false, "md5_hash": "x"},
            {"version": "310.9.16.0", "is_dev_file": true, "md5_hash": "y"},
            {"version": "310.9.16.0", "is_dev_file": false, "md5_hash": "z"}
        ], "dlss_g": [], "streamline": [
            {"version": "2.14.1", "is_dev_file": false, "file_description": "streamline"},
            {"version": "2.14.16", "is_dev_file": false, "file_description": "streamline"}
        ], "fsr_40_drv": [
            {"version": "4.0.0", "is_dev_file": false},
            {"version": "4.1.1", "is_dev_file": false}
        ]}"#;
        let versions = parse_versions(manifest).unwrap_or_default();
        assert_eq!(versions.dlss, vec!["310.9.16.0", "310.9.1.0"]);
        assert_eq!(versions.streamline, vec!["2.14.16", "2.14.1"]);
        assert_eq!(versions.fsr4, vec!["4.1.1", "4.0.0"]);
        assert!(versions.xess.is_empty());
    }
}
