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

/// Compiled in, so the pin always matches the harness that was built.
const LOCK: &str = include_str!("../images.lock");

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

/// The digest the lock pins `image` to. Same six columns as vendor/vendor.lock:
/// name, version, platform, kind, sha256, url — the reference is matched in the
/// url position, and only rows of kind `image` count.
fn digest_for<'a>(lock: &'a str, image: &str) -> Option<&'a str> {
    lock.lines().filter(|line| !line.trim_start().starts_with('#')).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            [_name, _version, _platform, "image", sha256, url] if *url == image => Some(*sha256),
            _ => None,
        }
    })
}

/// The reference a colony boots: `image` pinned by digest when the lock knows
/// it, otherwise `image` as written. Degrading to the bare tag rather than
/// failing is deliberate — a colony that cannot boot is worse than one booting
/// an unpinned tag, and the_real_lock_pins_every_preset below guarantees the
/// shipped lock pins every preset, so the degradation only ever fires for a
/// hand-edited lock or a half-shipped build.
pub fn pinned(image: &str) -> String {
    match digest_for(LOCK, image) {
        Some(digest) => format!("{image}@sha256:{digest}"),
        None => image.to_string(),
    }
}

/// The reference a preset's stack boots, for schema defaults that name a preset:
/// the same value `defaults` puts in `image`. `None` for `custom` or an unknown
/// id, which have no image of their own.
pub fn pinned_image(id: &str) -> Option<String> {
    find(id).map(|p| pinned(p.image))
}

/// The defaults a preset contributes, as a settings map.
///
/// Returns an empty map for `custom` or an unknown id, so a configuration
/// naming a preset that has since been removed degrades to the schema defaults
/// rather than failing to boot.
pub fn defaults(id: &str) -> Value {
    match find(id) {
        Some(p) => json!({
            "image": pinned(p.image),
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
        assert!(
            d["image"].as_str().unwrap().starts_with("node:24-bookworm@sha256:"),
            "{} is not the node:24-bookworm tag pinned by digest",
            d["image"]
        );
        assert_eq!(d["cpus"], 4);
        assert_eq!(d["memory"], "8G");
        assert_eq!(d["root_disk"], "16G");
    }

    const LOCK_SAMPLE: &str = "\
# a comment naming node 24-bookworm any image 9999 node:24-bookworm is not an entry
node      24-bookworm   any  image  1111  node:24-bookworm
python    3.13-bookworm any  image  2222  python:3.13-bookworm
golang    1-bookworm    any  binary 3333  golang:1-bookworm
";

    #[test]
    fn the_digest_lookup_picks_the_image_row_and_skips_the_rest() {
        assert_eq!(digest_for(LOCK_SAMPLE, "node:24-bookworm"), Some("1111"));
        assert_eq!(digest_for(LOCK_SAMPLE, "python:3.13-bookworm"), Some("2222"));
        // The reference also appears on a non-image row, which must not match.
        assert_eq!(digest_for(LOCK_SAMPLE, "golang:1-bookworm"), None, "a non-image kind never matches");
        assert_eq!(digest_for(LOCK_SAMPLE, "rust:1-bookworm"), None, "an unknown reference pins nothing");
    }

    #[test]
    fn an_unpinned_image_degrades_to_the_bare_reference() {
        // A colony that cannot boot is worse than one booting an unpinned tag;
        // the_real_lock_pins_every_preset is what keeps this path dormant.
        assert_eq!(pinned("ghcr.io/me/my-toolchain:1"), "ghcr.io/me/my-toolchain:1");
    }

    #[test]
    fn the_schema_default_is_the_preset_its_own_pinned_image() {
        // modules.rs takes its image default from here, so this is the node stack
        // default, not a second copy of it.
        assert_eq!(pinned_image("node").as_deref(), Some(pinned("node:24-bookworm").as_str()));
        assert_eq!(pinned_image(CUSTOM), None, "custom has no image of its own");
        assert_eq!(pinned_image("no-such-preset"), None);
    }

    #[test]
    fn the_real_lock_pins_every_preset() {
        for p in PRESETS {
            let reference = pinned(p.image);
            let digest = reference
                .strip_prefix(&format!("{}@sha256:", p.image))
                .unwrap_or_else(|| panic!("{}: {} is not pinned by images.lock", p.id, p.image));
            assert_eq!(digest.len(), 64, "{}: sha256 must be 64 hex characters", p.id);
            assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{}: {digest} is not hex", p.id);
        }
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
