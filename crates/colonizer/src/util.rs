//! Small process, file and string helpers shared across the harness.

use anyhow::{bail, Context, Result};
use std::{
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    process::Stdio,
};
use tokio::process::Command;

pub fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Renders a command for error messages without leaking environment values (tokens live in env).
pub fn describe(cmd: &Command) -> String {
    let std = cmd.as_std();
    let program = std.get_program().to_string_lossy().into_owned();
    let mut parts = vec![program.clone()];
    let mut args = std.get_args();
    while let Some(arg) = args.next() {
        if program == "git" && arg.to_str() == Some("-c") {
            args.next();
            continue;
        }
        parts.push(arg.to_string_lossy().into_owned());
    }
    parts.join(" ")
}

pub async fn exec(cmd: &mut Command) -> Result<String> {
    let desc = describe(cmd);
    let out = cmd
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("failed to start `{desc}`"))?;
    if !out.status.success() {
        bail!("`{desc}` failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a command for its exit status only.
pub async fn exec_status(cmd: &mut Command) -> Result<bool> {
    let desc = describe(cmd);
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .with_context(|| format!("failed to start `{desc}`"))?;
    Ok(status.success())
}

pub fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Writes a secret with 0600 permissions, tightening the parent directory to 0700.
pub fn write_secret(path: &Path, value: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    write_private(path, value.as_bytes())
}

/// Writes a file with 0600 permissions without touching the parent directory's mode.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    Ok(())
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

pub fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [owner, name].iter().all(|p| {
        !p.is_empty()
            && p.len() <= 100
            && *p != "."
            && *p != ".."
            && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    })
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn is_elf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"\x7fELF"
}

/// microsandbox mount specs are `SRC:DST[:OPTS]`, so paths must not contain separators.
pub fn mount_spec(src: &Path, dst: &str, read_only: bool) -> Result<String> {
    let s = src.display().to_string();
    if s.contains(':') || s.contains(',') {
        bail!("cannot mount {s}: path contains ':' or ','");
    }
    Ok(if read_only { format!("{s}:{dst}:ro") } else { format!("{s}:{dst}") })
}

pub fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// 244 bits of randomness, hex encoded.
pub fn random_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Parses a disk size the way the sandbox settings write them (`memory` is `"8G"`, `root_disk` is `"16G"`):
/// `512M`, `16G`, a bare byte count, a `K`/`M`/`G`/`T` suffix in either case. `""` and `"0"` parse to `0`,
/// which every reader treats as unlimited. `None` is malformed input: settings validation refuses it at
/// save time, so a stored value is unparseable only if the JSON was hand-edited, and readers then treat
/// the quota as unset rather than guessing at what was meant.
pub fn parse_disk_size(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    let (digits, unit) = match text.as_bytes()[text.len() - 1] {
        b'0'..=b'9' => (text, 1),
        b'K' | b'k' => (&text[..text.len() - 1], 1 << 10),
        b'M' | b'm' => (&text[..text.len() - 1], 1 << 20),
        b'G' | b'g' => (&text[..text.len() - 1], 1 << 30),
        b'T' | b't' => (&text[..text.len() - 1], 1 << 40),
        _ => return None,
    };
    digits.parse::<u64>().ok()?.checked_mul(unit)
}

/// The inverse of [`parse_disk_size`], for messages: the largest unit that fits, one decimal when there is
/// a fraction — `16G`, `1.5G`, `512M`, `4K`, `512B`.
pub fn format_disk_size(bytes: u64) -> String {
    let Some((unit, suffix)) = [(1u64 << 40, "T"), (1 << 30, "G"), (1 << 20, "M"), (1 << 10, "K")]
        .into_iter()
        .find(|(unit, _)| bytes >= *unit)
    else {
        return format!("{bytes}B");
    };
    let (whole, tenths) = (bytes / unit, bytes % unit * 10 / unit);
    if tenths == 0 {
        format!("{whole}{suffix}")
    } else {
        format!("{whole}.{tenths}{suffix}")
    }
}

/// Sums the size of everything under `root`, never following symlinks (a symlink counts its own few
/// bytes, never its target's), so a walk of one directory cannot wander out of it. Whatever cannot be
/// read — a missing root, an entry deleted mid-walk, a permission error — contributes nothing: half a
/// measurement still says something, and none of it fails or panics. Hard links count once per name,
/// which is the right overestimate for a quota. The walk keeps its to-do list on the heap instead of
/// the call stack: the tree is the colony's own worktree, which the agent inside fully controls, and
/// a runaway build can nest directories far deeper than a thread's stack cares about.
pub fn dir_size(root: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(root) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            // DirEntry::metadata, like `symlink_metadata`, never follows the entry's own symlink.
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_names_accept_only_a_single_segment() {
        for good in ["ecc", "house-style", "team_rules", "v2.2.1"] {
            assert!(is_plain_name(good), "{good} should be accepted");
        }
    }

    #[test]
    fn plain_names_reject_anything_that_escapes_the_directory() {
        // Each of these is a way for a settings string to name something the
        // harness never intended to mount.
        for bad in [
            "",
            "..",
            "../../etc",
            "a/b",
            "a\\b",
            ".hidden",
            "has:colon",
            "has,comma",
            "/absolute",
        ] {
            assert!(!is_plain_name(bad), "{bad:?} should be rejected");
        }
    }

    use super::*;

    #[test]
    fn repo_names_are_validated() {
        assert!(valid_repo("owner/repo.name-1"));
        assert!(valid_repo("owner/.github"));
        assert!(!valid_repo("../../etc"));
        assert!(!valid_repo("owner/.."));
        assert!(!valid_repo("owner/repo/extra"));
        assert!(!valid_repo("owner"));
    }

    #[test]
    fn mount_specs_reject_separators() {
        assert_eq!(mount_spec(Path::new("/a/b"), "/c", true).unwrap(), "/a/b:/c:ro");
        assert!(mount_spec(Path::new("/a:b"), "/c", false).is_err());
    }

    #[test]
    fn disk_sizes_parse_with_their_suffixes_and_empty_or_zero_mean_unlimited() {
        assert_eq!(parse_disk_size("16G"), Some(16 * 1024 * 1024 * 1024));
        assert_eq!(parse_disk_size("512m"), Some(512 * 1024 * 1024), "the suffix is case-insensitive");
        assert_eq!(parse_disk_size("4K"), Some(4 * 1024));
        assert_eq!(parse_disk_size("2t"), Some(2u64 * 1024 * 1024 * 1024 * 1024));
        assert_eq!(parse_disk_size("1024"), Some(1024), "a bare number is bytes");
        assert_eq!(parse_disk_size(" 8G "), Some(8 * 1024 * 1024 * 1024), "surrounding space is trimmed");
        assert_eq!(parse_disk_size("0"), Some(0), "0 means unlimited");
        assert_eq!(parse_disk_size(""), Some(0), "and so does nothing set");
    }

    #[test]
    fn malformed_disk_sizes_are_rejected_not_guessed_at() {
        for bad in ["eight", "-1", "1.5G", "16 GB", "G", "16Gi", "999999999999T"] {
            assert_eq!(parse_disk_size(bad), None, "{bad:?} is not a size");
        }
    }

    #[test]
    fn disk_sizes_are_formatted_back_the_way_they_are_written() {
        assert_eq!(format_disk_size(0), "0B");
        assert_eq!(format_disk_size(512), "512B");
        assert_eq!(format_disk_size(4 * 1024 + 512), "4.5K", "one decimal when there is a fraction");
        assert_eq!(format_disk_size(512 * 1024 * 1024), "512M");
        assert_eq!(format_disk_size(1_610_612_736), "1.5G");
        assert_eq!(format_disk_size(16 * 1024 * 1024 * 1024), "16G");
    }

    #[test]
    fn dir_size_sums_a_tree_without_following_symlinks() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("colonizer-util-test-{}", short_id()));
        std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
        std::fs::write(root.join("nested/deeper/big"), vec![0u8; 4096]).unwrap();
        std::fs::write(root.join("nested/small"), vec![0u8; 10]).unwrap();
        // A symlink to the big file counts as the link itself, never its target; a dangling one counts too.
        symlink(root.join("nested/deeper/big"), root.join("link")).unwrap();
        symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        let linked = std::fs::symlink_metadata(root.join("link")).unwrap().len();
        let dangling = std::fs::symlink_metadata(root.join("dangling")).unwrap().len();
        assert_eq!(dir_size(&root), 4096 + 10 + linked + dangling, "the targets of symlinks stay out of the sum");
        assert_eq!(dir_size(&root.join("nowhere")), 0, "a missing root measures nothing instead of failing");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dir_size_walks_a_very_deep_tree_without_blowing_the_stack() {
        let root = std::env::temp_dir().join(format!("colonizer-util-test-{}", short_id()));
        std::fs::create_dir_all(&root).unwrap();
        // The kernel refuses any single path longer than ~4K bytes, so one-letter names bought with that
        // budget go about as deep as any path-walking walk can follow on Linux — nearly two thousand
        // directories. A runaway build inside a colony can nest this deep, which a recursive walk would
        // ride down until the thread's stack gave out and took the mothership with it.
        let levels = 4_000usize.saturating_sub(root.as_os_str().len()) / 2;
        let mut here = root.clone();
        for _ in 0..levels {
            here.push("d");
            std::fs::create_dir(&here).unwrap();
        }
        std::fs::write(here.join("bottom"), b"deep payload").unwrap();
        assert!(dir_size(&root) >= "deep payload".len() as u64, "a tree {levels} directories deep is summed right down to its bottom file");
        // Take it apart the way it was built: absolute paths to the deepest levels no longer fit in the 4K budget.
        std::fs::remove_file(here.join("bottom")).unwrap();
        while here != root {
            std::fs::remove_dir(&here).unwrap();
            here.pop();
        }
        std::fs::remove_dir(&root).unwrap();
    }
}

/// A single path segment with no separators, no traversal and no leading dot.
///
/// Used where a setting names something the harness will resolve under a
/// directory it owns: a name that is allowed to contain `/` or `..` is a way to
/// reach the rest of the host's filesystem.
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains(':')
        && !name.contains(',')
        && !name.contains('\0')
}
