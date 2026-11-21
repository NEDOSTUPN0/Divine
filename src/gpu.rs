//! Which graphics cards the machine has, as far as the upscaler settings care.
//!
//! The vendor comes from the PCI ids under /sys/class/drm, which every kernel
//! driver fills in. An NVIDIA card's generation does not: NVIDIA ties its
//! features to the CUDA compute capability (7.5 Turing, 8.9 Ada, 12.0
//! Blackwell), and nvidia-smi is what reports it.
//!
//! An answer that cannot be had counts as yes. A card Divine fails to read
//! should not lose settings it may well support.

use std::fs;
use std::process::Command;
use std::sync::OnceLock;

const NVIDIA: &str = "0x10de";
const AMD: &str = "0x1002";
const INTEL: &str = "0x8086";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gpus {
    pub nvidia: Option<NvidiaCard>,
    pub amd: bool,
    pub intel: bool,
}

/// The most capable NVIDIA card present.
#[derive(Debug, Clone, PartialEq)]
pub struct NvidiaCard {
    pub name: String,
    /// `None` when nvidia-smi gave no answer.
    pub compute: Option<(u32, u32)>,
}

impl NvidiaCard {
    fn at_least(&self, wanted: (u32, u32)) -> bool {
        self.compute.is_none_or(|compute| compute >= wanted)
    }

    /// DLSS needs tensor cores: an RTX card, Turing or later. The GTX 16
    /// series is Turing too, and has none.
    pub fn dlss(&self) -> bool {
        self.at_least((7, 5)) && !self.name.contains("GTX")
    }

    /// Frame generation, DLSS's and Smooth Motion alike: RTX 40 and later.
    pub fn frame_generation(&self) -> bool {
        self.dlss() && self.at_least((8, 9))
    }

    /// More than one generated frame per rendered one, and the dynamic
    /// multiplier: RTX 50.
    pub fn multi_frame_generation(&self) -> bool {
        self.dlss() && self.at_least((10, 0))
    }
}

/// Read once: cards do not come and go while Divine runs. Blocking the first
/// time, for as long as nvidia-smi takes to answer.
pub fn detected() -> &'static Gpus {
    static GPUS: OnceLock<Gpus> = OnceLock::new();
    GPUS.get_or_init(detect)
}

fn detect() -> Gpus {
    let vendors: Vec<String> = fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|card| fs::read_to_string(card.path().join("device/vendor")).ok())
        .map(|vendor| vendor.trim().to_string())
        .collect();
    let has = |id: &str| vendors.iter().any(|vendor| vendor == id);

    Gpus {
        nvidia: has(NVIDIA).then(nvidia_card),
        amd: has(AMD),
        intel: has(INTEL),
    }
}

fn nvidia_card() -> NvidiaCard {
    let answer = Command::new("nvidia-smi")
        .args(["--query-gpu=name,compute_cap", "--format=csv,noheader"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    best_card(&answer).unwrap_or(NvidiaCard {
        name: String::new(),
        compute: None,
    })
}

/// One line per card, `NVIDIA GeForce RTX 5070, 12.0`.
fn best_card(answer: &str) -> Option<NvidiaCard> {
    answer
        .lines()
        .filter_map(|line| {
            let (name, compute) = line.rsplit_once(',')?;
            let (major, minor) = compute.trim().split_once('.')?;
            Some(NvidiaCard {
                name: name.trim().to_string(),
                compute: Some((major.parse().ok()?, minor.parse().ok()?)),
            })
        })
        .max_by_key(|card| card.compute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_capable_card_is_the_one_that_counts() {
        let card = best_card("NVIDIA GeForce RTX 3060, 8.6\nNVIDIA GeForce RTX 5070, 12.0\n");
        assert_eq!(
            card.map(|card| card.name),
            Some("NVIDIA GeForce RTX 5070".to_string())
        );
    }

    #[test]
    fn each_generation_gets_its_own_features() {
        let card = |name: &str, compute| NvidiaCard {
            name: name.into(),
            compute: Some(compute),
        };
        let rtx_30 = card("NVIDIA GeForce RTX 3080", (8, 6));
        assert!(rtx_30.dlss() && !rtx_30.frame_generation());
        let rtx_40 = card("NVIDIA GeForce RTX 4070", (8, 9));
        assert!(rtx_40.frame_generation() && !rtx_40.multi_frame_generation());
        let rtx_50 = card("NVIDIA GeForce RTX 5070", (12, 0));
        assert!(rtx_50.multi_frame_generation());
    }

    #[test]
    fn a_gtx_card_has_no_dlss_whatever_its_generation() {
        let gtx = NvidiaCard {
            name: "NVIDIA GeForce GTX 1660 SUPER".into(),
            compute: Some((7, 5)),
        };
        assert!(!gtx.dlss());
    }

    #[test]
    fn a_card_that_could_not_be_read_keeps_everything() {
        let unknown = NvidiaCard {
            name: String::new(),
            compute: None,
        };
        assert!(unknown.multi_frame_generation());
    }
}
