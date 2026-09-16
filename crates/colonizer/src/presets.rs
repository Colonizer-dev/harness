//! Named sandbox presets.
//!
//! `sandbox.image` is free text, and before this a Python or Go user had to know
//! three separate things before their first colony worked: that the field
//! exists, what to put in it, and that the image has to be glibc-based — a
//! constraint documented under "What this does not do" rather than next to the
//! field. A preset is a stack you pick instead of an image tag you type, and
//! every preset already satisfies the glibc constraint.
//!
//! Presets only supply defaults. Anything set explicitly in `modules.json`
//! still wins, so an existing configuration keeps the image it had.

use serde_json::{json, Value};

/// A named bundle of image and machine size.
pub struct Preset {
    pub id: &'static str,
    pub image: &'static str,
    pub cpus: u64,
    pub memory: &'static str,
    pub root_disk: &'static str,
}

/// `custom` is not in this table: it means "use the fields as written", which is
/// how the sandbox behaved before presets existed.
///
/// `node` reproduces the previous defaults exactly, so an install that never
/// touches this setting boots the same colony it booted before.
pub const PRESETS: &[Preset] = &[
    Preset { id: "node", image: "node:24-bookworm", cpus: 4, memory: "8G", root_disk: "16G" },
    Preset { id: "python", image: "python:3.13-bookworm", cpus: 4, memory: "8G", root_disk: "16G" },
    // Rust builds are memory and disk hungry in a way the others are not.
    Preset { id: "rust", image: "rust:1-bookworm", cpus: 6, memory: "12G", root_disk: "32G" },
    Preset { id: "go", image: "golang:1-bookworm", cpus: 4, memory: "8G", root_disk: "16G" },
];

pub const CUSTOM: &str = "custom";

/// The preset ids offered in Settings, in order, with `custom` last.
pub fn ids() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.id).chain(std::iter::once(CUSTOM)).collect()
}

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The defaults a preset contributes, as a settings map.
///
/// Returns an empty map for `custom` or an unknown id, so a configuration
/// naming a preset that has since been removed degrades to the schema defaults
/// rather than failing to boot.
pub fn defaults(id: &str) -> Value {
    match find(id) {
        Some(p) => json!({
            "image": p.image,
            "cpus": p.cpus,
            "memory": p.memory,
            "root_disk": p.root_disk,
        }),
        None => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_reproduces_the_previous_defaults() {
        // These four were the schema defaults before presets existed. If this
        // fails, every existing install silently changes machine on upgrade.
        let d = defaults("node");
        assert_eq!(d["image"], "node:24-bookworm");
        assert_eq!(d["cpus"], 4);
        assert_eq!(d["memory"], "8G");
        assert_eq!(d["root_disk"], "16G");
    }

    #[test]
    fn custom_contributes_nothing() {
        assert_eq!(defaults(CUSTOM), json!({}));
    }

    #[test]
    fn an_unknown_preset_degrades_instead_of_failing() {
        assert_eq!(defaults("no-such-preset"), json!({}));
    }

    #[test]
    fn every_preset_is_glibc_based() {
        // The host's native Claude Code binary is mounted into the colony, so a
        // musl image (alpine) cannot run it. A preset that needs the user to
        // know that has failed at its job.
        for p in PRESETS {
            assert!(
                p.image.contains("bookworm") || p.image.contains("trixie") || p.image.contains("slim"),
                "preset {} uses {}, which is not obviously a glibc image",
                p.id,
                p.image
            );
            assert!(!p.image.contains("alpine"), "preset {} is musl-based", p.id);
        }
    }

    #[test]
    fn ids_offer_every_preset_and_custom_last() {
        let ids = ids();
        assert_eq!(ids.last(), Some(&CUSTOM));
        for p in PRESETS {
            assert!(ids.contains(&p.id), "{} missing from the picker", p.id);
        }
    }

    #[test]
    fn every_preset_has_a_sane_machine() {
        for p in PRESETS {
            assert!(p.cpus >= 1, "{} has no vCPUs", p.id);
            assert!(p.memory.ends_with('G'), "{} memory {} is not a size", p.id, p.memory);
            assert!(p.root_disk.ends_with('G'), "{} root disk {} is not a size", p.id, p.root_disk);
        }
    }
}
