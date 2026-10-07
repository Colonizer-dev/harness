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

use serde_json::{Value, json};

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
    Preset {
        id: "node",
        image: "node:24-bookworm",
        cpus: 4,
        memory: "8G",
        root_disk: "16G",
    },
    Preset {
        id: "python",
        image: "python:3.13-bookworm",
        cpus: 4,
        memory: "8G",
        root_disk: "16G",
    },
    // Rust builds are memory and disk hungry in a way the others are not.
    Preset {
        id: "rust",
        image: "rust:1-bookworm",
        cpus: 6,
        memory: "12G",
        root_disk: "32G",
    },
    Preset {
        id: "go",
        image: "golang:1-bookworm",
        cpus: 4,
        memory: "8G",
        root_disk: "16G",
    },
];

pub const CUSTOM: &str = "custom";

/// The id meaning "detect the stack from the repository" instead of naming one.
/// It is offered first in Settings; wherever there is no repository to look at,
/// it resolves through [`resolved`] to [`AUTO_FALLBACK`].
pub const AUTO: &str = "auto";

/// What `auto` falls back to when a repository names no stack: exactly what
/// colonies booted before detection existed.
pub const AUTO_FALLBACK: &str = "node";

/// The preset ids offered in Settings, in order: `auto` first so detection is
/// the visible default, then every preset, then `custom` last.
pub fn ids() -> Vec<&'static str> {
    std::iter::once(AUTO)
        .chain(PRESETS.iter().map(|p| p.id))
        .chain(std::iter::once(CUSTOM))
        .collect()
}

/// The stack an id names when there is no repository to look at: `auto` has
/// nothing to detect from, so it means its fallback, and any other id names
/// itself. This exists because the Setup pane's image pre-pull and the
/// telemetry baseline run with no worktree in hand.
pub fn resolved(id: &str) -> &str {
    if id == AUTO { AUTO_FALLBACK } else { id }
}

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The digest the lock pins `image` to. Same six columns as vendor/vendor.lock:
/// name, version, platform, kind, sha256, url — the reference is matched in the
/// url position, and only rows of kind `image` count.
fn digest_for<'a>(lock: &'a str, image: &str) -> Option<&'a str> {
    lock.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
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

/// The file names that name a stack, and the preset each one means.
///
/// The ORDER here is the tie-break at equal depth, and it is chosen by what a
/// wrong guess costs: a compiled toolchain is the expensive one to add to a
/// running colony, while a Node toolchain is an apt-get away from any of the
/// others, and the reverse is not true. So when a repository is both, it is
/// treated as the compiled one.
const MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),
    ("go.mod", "go"),
    ("pyproject.toml", "python"),
    ("requirements.txt", "python"),
    ("setup.py", "python"),
    ("Pipfile", "python"),
    ("package.json", "node"),
];

/// A detection result: not just the answer but the file that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// The preset id the marker names.
    pub stack: &'static str,
    /// The marker that chose it, repo-relative, so a wrong guess is diagnosable
    /// from the log.
    pub marker: String,
}

/// The stack a set of files names, or `None` when none of them is a marker, in
/// which case the caller keeps its configured stack rather than guessing.
///
/// `paths` are repo-relative and `/`-separated. A path matches when its last
/// `/`-separated segment equals a marker name. A marker at the root beats one
/// in a subdirectory, so a Rust service with a `web/package.json` front end is
/// Rust; at equal depth the one earlier in [`MARKERS`] wins.
///
/// Matches rank by `(is_not_root, marker_index, path)` and the minimum wins,
/// so the answer never depends on the order `paths` arrives in.
pub fn detect<S: AsRef<str>>(paths: &[S]) -> Option<Detected> {
    paths
        .iter()
        .filter_map(|path| {
            let path = path.as_ref();
            let name = match path.rsplit_once('/') {
                Some((_dir, name)) => name,
                None => path,
            };
            MARKERS
                .iter()
                .position(|(marker, _)| *marker == name)
                .map(|index| (path.contains('/'), index, path))
        })
        .min()
        .map(|(_not_root, index, path)| Detected {
            stack: MARKERS[index].1,
            marker: path.to_string(),
        })
}

/// [`detect`] over a real repository: markers at the root, and one level below,
/// where a monorepo's `web/package.json` sits. Only marker file names are
/// collected, never a whole listing, and dot-directories and build output are
/// skipped on the way down.
///
/// An unreadable directory yields no markers rather than an error: the caller
/// falls back to its configured stack when detection finds nothing, so a tree
/// that cannot be read must never stop a colony booting.
pub fn detect_in(root: &std::path::Path) -> Option<Detected> {
    // VCS state and build output say nothing about the stack, and `target/`
    // alone can hold tens of thousands of entries.
    const SKIP: &[&str] = &["node_modules", "target", "dist", "build", "vendor"];

    fn collect(dir: &std::path::Path, prefix: &str, depth: usize, paths: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else { continue };
            if file_type.is_dir() {
                // The walk covers the root and one level below it, no deeper.
                if depth == 0 && !name.starts_with('.') && !SKIP.contains(&name) {
                    collect(&dir.join(name), &format!("{prefix}{name}/"), depth + 1, paths);
                }
            } else if MARKERS.iter().any(|(marker, _)| *marker == name) {
                paths.push(format!("{prefix}{name}"));
            }
        }
    }

    let mut paths = Vec::new();
    collect(root, "", 0, &mut paths);
    detect(&paths)
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
        assert_eq!(
            digest_for(LOCK_SAMPLE, "golang:1-bookworm"),
            None,
            "a non-image kind never matches"
        );
        assert_eq!(
            digest_for(LOCK_SAMPLE, "rust:1-bookworm"),
            None,
            "an unknown reference pins nothing"
        );
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
    fn ids_offer_auto_first_then_presets_then_custom_last() {
        let ids = ids();
        assert_eq!(ids.first(), Some(&AUTO), "auto should be the visible default");
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

    #[test]
    fn a_root_marker_beats_a_front_end_in_a_subdirectory() {
        let d = detect(&["Cargo.toml", "src/main.rs", "web/package.json", "README.md"]).expect("Cargo.toml is a marker");
        assert_eq!(d.stack, "rust", "the root Cargo.toml should win over web/package.json");
        assert_eq!(d.marker, "Cargo.toml");
    }

    #[test]
    fn no_markers_means_no_guess() {
        assert_eq!(detect(&["README.md", "src/main.rs", "docker-compose.yml"]), None);
    }

    #[test]
    fn a_root_marker_beats_a_deeper_one_of_any_kind() {
        let d = detect(&["backend/Cargo.toml", "package.json"]).expect("package.json is a marker");
        assert_eq!(d.stack, "node", "the root package.json should win over backend/Cargo.toml");
        assert_eq!(d.marker, "package.json");
    }

    #[test]
    fn a_marker_only_in_a_subdirectory_is_still_found() {
        let d = detect(&["backend/pyproject.toml"]).expect("pyproject.toml is a marker");
        assert_eq!(d.stack, "python");
        assert_eq!(d.marker, "backend/pyproject.toml");
    }

    #[test]
    fn two_root_markers_tie_break_by_table_order_not_argument_order() {
        let first = detect(&["package.json", "go.mod"]).expect("go.mod is a marker");
        let second = detect(&["go.mod", "package.json"]).expect("package.json is a marker");
        assert_eq!(first.stack, "go", "go.mod is earlier in MARKERS than package.json");
        assert_eq!(first, second, "the answer must not depend on the order paths arrive in");
    }

    #[test]
    fn every_python_marker_names_python() {
        let names: Vec<&str> = MARKERS
            .iter()
            .filter(|(_, stack)| *stack == "python")
            .map(|(name, _)| *name)
            .collect();
        assert!(!names.is_empty(), "MARKERS no longer names python at all");
        for name in names {
            let d = detect(&[name]).unwrap_or_else(|| panic!("{name} is a marker"));
            assert_eq!(d.stack, "python", "{name} should map to python");
            assert_eq!(d.marker, name);
        }
    }

    #[test]
    fn resolved_maps_auto_to_the_fallback_and_everything_else_to_itself() {
        assert_eq!(resolved(AUTO), AUTO_FALLBACK);
        assert_eq!(resolved("rust"), "rust");
        assert_eq!(resolved(CUSTOM), CUSTOM);
    }

    #[test]
    fn every_marker_stack_and_the_auto_fallback_is_a_real_preset() {
        for (name, stack) in MARKERS {
            assert!(find(stack).is_some(), "{name} names {stack}, which is not a preset");
        }
        assert!(find(AUTO_FALLBACK).is_some(), "the auto fallback must be a preset");
    }

    #[test]
    fn detect_in_reads_the_root_and_one_level_below() {
        let root = std::env::temp_dir().join(format!("colonizer-presets-detect-{}", crate::util::short_id()));
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"detect-in-test\"\n").unwrap();
        std::fs::write(root.join("web/package.json"), "{}").unwrap();
        let d = detect_in(&root).expect("a rust repo with a web front end");
        assert_eq!(d.stack, "rust", "the root Cargo.toml should win over web/package.json");
        assert_eq!(d.marker, "Cargo.toml");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_in_yields_none_for_a_tree_it_cannot_read() {
        let nowhere = std::env::temp_dir().join(format!("colonizer-presets-absent-{}", crate::util::short_id()));
        assert_eq!(detect_in(&nowhere), None);
    }
}
