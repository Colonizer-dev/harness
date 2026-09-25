//! Path policy (issue #300): which worktree paths a colony must never read (`masked`) and which it
//! may read but never write (`protected`), what the host prepares before the microVM starts, and
//! what the guest enforces as it boots — before the agent can run a single command.
//! docs/path-policy.md is the operator-facing story.

use crate::config::{ModuleChoice, setting};
use serde_json::Value;
use std::{collections::HashSet, io, path::Path, path::PathBuf};

/// Inside the session's `vm/` directory — the colony's read-only `/colonizer` — the list of guest
/// enforcement actions, one `kind path` pair per line. The guest loop in `BOOT_SCRIPT` consumes it;
/// publish reads it back, so it checks the colony's work against exactly the policy it saw.
pub(crate) const POLICY_FILE: &str = "path-policy";
/// Sibling of [`POLICY_FILE`]: the placeholders [`plan`] decided on (created by [`apply`], or found
/// still empty from an earlier boot of the same colony), so publish can remove the still-empty ones
/// before staging.
pub(crate) const PLACEHOLDERS_FILE: &str = "path-policy.placeholders";

/// The worktree files that carry credentials by long convention, hidden from the colony outright.
pub(crate) const DEFAULT_MASKED: &[(&str, &str)] = &[
    (".env", "dotenv secrets"),
    (".envrc", "direnv exports"),
    (".npmrc", "registry tokens"),
    (".netrc", "host logins"),
    (".git-credentials", "stored git credentials"),
    (".pypirc", "package index tokens"),
];

/// Agent- and workspace-facing config the colony may consult but not rewrite: its own leash, in
/// other words. Entries under `.git` are listed for the record but need no bind of their own — the
/// boot mounts the whole git admin dir read-only at its host path already.
pub(crate) const DEFAULT_PROTECTED: &[(&str, &str)] = &[
    (".git/config", "git's own config"),
    (".git/hooks/", "git hooks, a code-execution path"),
    (".gitmodules", "what submodules would fetch"),
    (".claude/", "agent settings and hooks"),
    (".codex/", "agent settings and hooks"),
    (".mcp.json", "tool servers the agent would talk to"),
    (".devcontainer/", "container build config"),
    (".vscode/", "editor tasks and launch configs"),
    (".idea/", "editor tasks and run configs"),
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Policy {
    /// Paths whose contents the colony never sees; a trailing `/` means the whole directory.
    pub masked: Vec<String>,
    /// Paths the colony may read but not write; a trailing `/` means the whole directory.
    pub protected: Vec<String>,
    /// Opt-outs: removed from both sets above, defaults included. Each one is logged at boot.
    pub unmasked: Vec<String>,
}

/// The built-in policy, before settings add to it or opt out of it.
pub(crate) fn defaults() -> Policy {
    Policy {
        masked: DEFAULT_MASKED.iter().map(|(path, _)| path.to_string()).collect(),
        protected: DEFAULT_PROTECTED.iter().map(|(path, _)| path.to_string()).collect(),
        unmasked: Vec::new(),
    }
}

/// Reads the policy off a sandbox module choice plus its schema — the same resolved settings the
/// boot uses for image and memory. Entries are re-validated here rather than trusted: modules.json
/// is a file a hand can edit, and [`apply`] must never chase an absolute path out of the worktree.
/// Entries that fail are dropped, not fatal — the save-time gate is where an operator is told — and
/// the boot's summary line counts what will actually be enforced.
pub(crate) fn from_settings(choice: &ModuleChoice, schema: &Value) -> Policy {
    let list = |key: &str, masked: bool| -> Vec<String> {
        let usable = |p: &str| {
            if masked {
                validate_masked(p).is_ok()
            } else {
                validate_path(p).is_ok()
            }
        };
        setting(choice, schema, key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|p| usable(p))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut policy = defaults();
    // User entries join the built-ins; duplicates collapse, first occurrence wins. The built-ins
    // are never re-validated: they are consts of this module, and [`plan`] skips the `.git` ones
    // anyway.
    for (paths, user) in [
        (&mut policy.masked, list("mask_paths", true)),
        (&mut policy.protected, list("protect_paths", false)),
    ] {
        for entry in user {
            if !paths.iter().any(|p| same_path(p, &entry)) {
                paths.push(entry);
            }
        }
    }
    policy.unmasked = list("unmask_paths", false);
    // Mask wins: a path in both sets is only masked, whatever listed it first.
    policy.protected.retain(|p| !policy.masked.iter().any(|m| same_path(m, p)));
    // An opt-out is an explicit operator decision, so it clears the path out of both sets —
    // defaults and extras alike — and the boot logs each one (see `opt_outs`).
    policy.masked.retain(|p| !policy.unmasked.iter().any(|u| same_path(u, p)));
    policy.protected.retain(|p| !policy.unmasked.iter().any(|u| same_path(u, p)));
    policy
}

/// Path equality that ignores a trailing `/`: the same entry spelled as a file and as a directory
/// is one entry, so mask-wins and the opt-out both see it.
fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The boot's info line: what will be enforced, what was created for it, and what could not be —
/// so the colony record shows the policy without reading files. The opt-out sentence is the
/// caller's separate warn line.
pub(crate) fn summary(policy: &Policy, planned: &Materialized) -> String {
    let mut line = format!(
        "path policy: masking {} path(s), protecting {}; {} placeholder(s) in the worktree",
        policy.masked.len(),
        policy.protected.len(),
        planned.placeholders.len(),
    );
    if !planned.skipped.is_empty() {
        line.push_str(&format!(
            "; skipped (nothing bound, worth a look): {}",
            planned.skipped.join(", ")
        ));
    }
    line
}

/// The warn line for `unmask_paths`: each opt-out with the default's one-line rationale where it
/// has one, so a colony log explains itself years later.
pub(crate) fn opt_outs(policy: &Policy) -> Option<String> {
    if policy.unmasked.is_empty() {
        return None;
    }
    let named = policy
        .unmasked
        .iter()
        .map(|entry| {
            match DEFAULT_MASKED
                .iter()
                .chain(DEFAULT_PROTECTED.iter())
                .find(|(path, _)| same_path(path, entry))
            {
                Some((_, why)) => format!("{entry} ({why})"),
                None => entry.clone(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("path policy: unmasked by setting, so the colony sees them: {named}"))
}

/// The save-time gate (`validate_settings` calls it for every path-list entry): a path must be
/// relative to the worktree root, must not be the root itself, must carry no traversal and no
/// character the wiring cannot carry — `:` breaks a mount spec, `,` a mount's option list, control
/// characters the one-per-line list format, and leading or trailing whitespace would act on a
/// different path than the one saved (the guest's `read` strips it, so `" /"` would act on the
/// whole worktree). `.git` is refused for masked entries in [`validate_masked`]: the git dir must
/// stay readable for the colony to boot at all, and it is already read-only, so a mask there would
/// buy nothing and cost the boot.
pub(crate) fn validate_path(entry: &str) -> Result<(), String> {
    if entry.is_empty() {
        return Err("the path is empty".into());
    }
    if entry != entry.trim() {
        return Err("leading or trailing whitespace is not allowed in a path".into());
    }
    if entry.chars().any(|c| c.is_control() || c == ':' || c == ',') {
        return Err("control characters and `:` or `,` are not allowed in a path".into());
    }
    if entry.starts_with('/') {
        return Err("paths are relative to the worktree root; drop the leading `/`".into());
    }
    let normalized = entry.trim_end_matches('/');
    if normalized.is_empty() || normalized == "." {
        return Err("the worktree root itself cannot be listed".into());
    }
    match normalized.split('/').find(|c| matches!(*c, "" | "." | "..")) {
        Some("") => return Err("empty path component (a doubled `/`)".into()),
        Some(".") => return Err("`.` cannot appear in a path".into()),
        Some("..") => return Err("`..` cannot appear in a path".into()),
        _ => {}
    }
    Ok(())
}

/// [`validate_path`] plus the one rule only a masked entry has: nothing under the git dir.
pub(crate) fn validate_masked(entry: &str) -> Result<(), String> {
    validate_path(entry)?;
    let normalized = entry.trim_end_matches('/');
    if normalized == ".git" || normalized.starts_with(".git/") {
        return Err("masking inside `.git` would break the colony's git; the directory is already mounted read-only".into());
    }
    Ok(())
}

/// What [`plan`] decided, for [`apply`] and for the boot log.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Materialized {
    /// One `kind path` line per enforcement action, written to [`POLICY_FILE`]. A directory entry
    /// keeps its trailing `/` on purpose: publish rebuilds the policy from these lines, and the
    /// slash is what keeps it matching everything below itself.
    pub binds: Vec<String>,
    /// `(worktree-relative path, is a directory)` to create empty — after this plan is on disk,
    /// never before (see [`apply`]).
    pub placeholders: Vec<(String, bool)>,
    /// Entries nothing can be safely bound for, reported on the boot log rather than half-enforced.
    pub skipped: Vec<String>,
}

impl Materialized {
    /// The placeholder paths, in plan order: what the boot writes to [`PLACEHOLDERS_FILE`] and a
    /// resume reads back.
    pub(crate) fn placeholder_names(&self) -> Vec<String> {
        self.placeholders.iter().map(|(rel, _)| rel.clone()).collect()
    }
}

/// One path per line, newline-terminated — the format [`read_list`] reads back.
pub(crate) fn write_list(path: &Path, lines: &[String]) -> io::Result<()> {
    std::fs::write(path, lines.iter().map(|l| format!("{l}\n")).collect::<String>())
}

/// What the host decided about one entry, before the VM ever runs.
enum Planned {
    /// A bind for this worktree-relative path — the entry itself, or, when the entry is a symlink
    /// that stays inside the worktree, the path it resolves to. `placeholder` is set when the path
    /// does not exist and [`apply`] must create it first.
    Bind {
        rel: String,
        dir: bool,
        placeholder: Option<bool>,
    },
    /// Nothing can be bound: a parent is not a real directory, or the path is a symlink to outside
    /// the worktree (or dangling). Reported, never half-enforced.
    Skip,
}

impl Planned {
    /// The entry is absent, or its parent chain is: a bind whose placeholder [`apply`] creates,
    /// file or directory per how the entry was spelled.
    fn placeholder(rel: &str, dir: bool) -> Planned {
        Planned::Bind {
            rel: rel.to_string(),
            dir,
            placeholder: Some(dir),
        }
    }
}

/// Works the policy against the checkout on the host, before the VM exists, without writing
/// anything: every listed path the checkout does not have is planned as an empty placeholder —
/// file, or directory for a `/` entry — because the guest's bind mount needs a target; paths that
/// exist are left as they are, the guest's bind enforcing over them. Symlinks are resolved here,
/// while nothing is running: a link that stays inside the worktree is bound at its target, one
/// that leaves the worktree or dangles (as does any entry whose parent chain runs through a link)
/// is reported as skipped — one odd entry must not fail a boot. `previous` carries the earlier
/// boot's placeholder list, so a resume keeps still-empty placeholders on the books without
/// re-claiming files it never created.
pub(crate) fn plan(worktree: &Path, policy: &Policy, previous: &[String]) -> io::Result<Materialized> {
    let mut out = Materialized::default();
    // Canonical forms on both sides, so a resolved target can be judged in-worktree without
    // trusting the worktree's own spelling.
    let root = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let mut recorded = HashSet::new();
    let entries = policy
        .masked
        .iter()
        .map(|rel| (rel, true))
        .chain(policy.protected.iter().map(|rel| (rel, false)));
    for (rel, masked) in entries {
        // Enforced by the git-dir mount, not by a bind — `validate_masked` keeps masked entries
        // out of `.git` in the first place, and protected ones have nothing to add here. The
        // revalidate is belt and braces against a hand-edited modules.json.
        if rel == ".git" || rel.starts_with(".git/") || validate_path(rel).is_err() {
            continue;
        }
        match plan_one(worktree, &root, rel)? {
            Planned::Skip => out.skipped.push(rel.clone()),
            Planned::Bind { rel, dir, placeholder } => {
                // The kind follows what is actually on disk, not how the entry was spelled: an
                // entry naming a file that turns out to be a directory masks as a directory, or
                // the guest's file bind would fail and brick the boot.
                let kind = match (masked, dir) {
                    (true, true) => "mask-dir",
                    (true, false) => "mask-file",
                    (false, _) => "protect",
                };
                let mut target = rel.clone();
                if dir && !target.ends_with('/') {
                    target.push('/');
                }
                out.binds.push(format!("{kind} {target}"));
                if let Some(dir) = placeholder {
                    out.placeholders.push((rel.clone(), dir));
                    recorded.insert(rel.clone());
                }
            }
        }
    }
    for rel in previous {
        if recorded.contains(rel) {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(worktree.join(rel))
            && is_empty(&worktree.join(rel), &meta)
        {
            out.placeholders.push((rel.clone(), meta.is_dir()));
        }
    }
    Ok(out)
}

/// The walk to an entry's parent directory: the real directory to sit in, a chain with a missing
/// link (the leaf is absent too), or something in the way. With `create`, a missing link is made
/// as it is met — how [`apply`] builds a placeholder's parents; [`plan`] passes `false`.
enum Walk {
    Parent(PathBuf),
    Missing,
    Blocked,
}

fn walk_parents(worktree: &Path, rel: &str, create: bool) -> io::Result<Walk> {
    let mut components: Vec<_> = Path::new(rel).components().collect();
    components.pop().expect("validated paths have a last component");
    let mut cur = worktree.to_path_buf();
    for component in components {
        cur.push(component);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.is_dir() => {}
            // A symlink or a plain file in the way: the entry cannot be reached as a plain path,
            // and a bind over a path reached through a link would not stay put.
            Ok(_) => return Ok(Walk::Blocked),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !create {
                    return Ok(Walk::Missing);
                }
                std::fs::create_dir(&cur)?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Walk::Parent(cur))
}

/// Decides one entry without touching anything. A missing parent means the leaf is absent too, so
/// it plans as a placeholder and [`apply`] creates the parents with it.
fn plan_one(worktree: &Path, root: &Path, rel: &str) -> io::Result<Planned> {
    let parent = match walk_parents(worktree, rel, false)? {
        Walk::Parent(dir) => dir,
        Walk::Missing => return Ok(Planned::placeholder(rel, rel.ends_with('/'))),
        Walk::Blocked => return Ok(Planned::Skip),
    };
    let path = parent.join(Path::new(rel).file_name().expect("validated paths have a last component"));
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Planned::placeholder(rel, rel.ends_with('/'))),
        Err(e) => Err(e),
        Ok(meta) if meta.is_symlink() => match std::fs::canonicalize(&path) {
            // In the worktree and real: bind the target, so masking `.env` masks the bytes the
            // checkout would actually serve.
            Ok(target) if target.starts_with(root) => {
                let rel = target
                    .strip_prefix(root)
                    .expect("checked above")
                    .to_string_lossy()
                    .into_owned();
                Ok(Planned::Bind {
                    rel,
                    dir: target.is_dir(),
                    placeholder: None,
                })
            }
            // Leaving the worktree, or a dangling link: nothing safe to bind.
            _ => Ok(Planned::Skip),
        },
        Ok(meta) => Ok(Planned::Bind {
            rel: rel.to_string(),
            dir: meta.is_dir(),
            placeholder: None,
        }),
    }
}

/// Creates what [`plan`] decided on — and only after the boot has written the intended placeholder
/// list to disk, so a placeholder that gets created is always one publish knows to remove. Best
/// effort per entry: a path that grew a symlink between the plan and here is skipped rather than
/// written through.
pub(crate) fn apply(worktree: &Path, planned: &Materialized) -> io::Result<()> {
    for (rel, dir) in &planned.placeholders {
        let Walk::Parent(parent) = walk_parents(worktree, rel, true)? else {
            continue;
        };
        let path = parent.join(Path::new(rel).file_name().expect("validated paths have a last component"));
        if std::fs::symlink_metadata(&path).is_ok() {
            continue;
        }
        if *dir {
            std::fs::create_dir(&path)?;
        } else {
            std::fs::write(&path, b"")?;
        }
    }
    Ok(())
}

/// Removes placeholders that are still empty — a masked path's placeholder is an empty file the
/// colony cannot write through its `/dev/null` bind, so "still empty" means "never real work" —
/// and returns what it removed. Anything the colony managed to fill, and anything missing, is
/// left as it is: the list only ever names paths a boot put there.
pub(crate) fn clean_placeholders(worktree: &Path, list: &Path) -> io::Result<Vec<String>> {
    Ok(remove_empty(
        worktree,
        &read_list(&std::fs::read_to_string(list).unwrap_or_default()),
        &[],
    ))
}

/// The publish-side safety net behind [`clean_placeholders`]: any empty regular file or empty
/// directory sitting at a policy path is removed too, even when the boot's placeholder list was
/// lost — but only when git does not track it (`tracked` is the `git ls-files` answer for exactly
/// these paths, taken by the caller). A tracked path is never touched, a symlink is never followed
/// ([`safe_path`] refuses any symlink component), and anything with content stays.
pub(crate) fn remove_empty_untracked(worktree: &Path, paths: &[String], tracked: &[String]) -> Vec<String> {
    remove_empty(worktree, paths, tracked)
}

/// The shared body: for each path `skip` does not name (modulo a trailing `/`), remove the empty
/// regular file or empty directory at it and return the ones removed.
fn remove_empty(worktree: &Path, paths: &[String], skip: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|rel| !skip.iter().any(|s| same_path(s, rel)))
        .filter_map(|rel| {
            let path = safe_path(worktree, rel)?;
            let meta = std::fs::symlink_metadata(&path).ok()?;
            let gone = is_empty(&path, &meta)
                && if meta.is_dir() {
                    std::fs::remove_dir(&path).is_ok()
                } else {
                    std::fs::remove_file(&path).is_ok()
                };
            gone.then(|| rel.clone())
        })
        .collect()
}

/// The worktree path for `rel` when every component along the way is a real, non-symlink name —
/// `None` when any component is a symlink, missing, or a parent is not a directory. Cleaning never
/// follows links: a placeholder is a plain host-side name the boot wrote, nothing else.
fn safe_path(worktree: &Path, rel: &str) -> Option<PathBuf> {
    let mut cur = worktree.to_path_buf();
    for component in Path::new(rel).components() {
        cur.push(component);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.is_symlink() => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    Some(cur)
}

/// Empty means never real work: no entries for a directory, zero bytes for a regular file.
fn is_empty(path: &Path, meta: &std::fs::Metadata) -> bool {
    if meta.is_dir() {
        std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_none())
    } else {
        meta.is_file() && meta.len() == 0
    }
}

/// The non-empty, trimmed lines of a list file: the placeholder list, or a changed-paths dump.
pub(crate) fn read_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Splits git's NUL-separated `-z` output, tolerating the trailing NUL.
pub(crate) fn z_paths(out: &str) -> Vec<String> {
    out.split('\0').filter(|path| !path.is_empty()).map(str::to_string).collect()
}

/// Rebuilds the policy a boot wrote, from the bind list the guest consumed — publish checks
/// against what the colony actually saw, not against a fresh settings read that could disagree.
/// Directory entries keep the trailing `/` the bind carries, so their below-itself matching
/// survives the round trip.
pub(crate) fn from_bind_list(text: &str) -> Policy {
    let mut policy = Policy::default();
    for line in read_list(text) {
        let Some((kind, rel)) = line.split_once(' ') else { continue };
        match kind {
            "mask-file" | "mask-dir" => policy.masked.push(rel.to_string()),
            "protect" => policy.protected.push(rel.to_string()),
            _ => {}
        }
    }
    policy
}

/// The changed paths that touch a masked or protected entry, each as a ready-to-log line naming
/// the path and which side of the policy it crossed. Matching is by whole components, at any
/// depth — a nested checkout's `vendor/lib/.env` is still `.env`, and a directory entry covers
/// everything below it — so a symlink at the path changes nothing: a changed path is a changed
/// path. Reporting only, by design: what gets committed is publish's business (issue #300).
pub(crate) fn violations(changed: &[String], policy: &Policy) -> Vec<String> {
    changed
        .iter()
        .filter_map(|path| {
            let kind = if matches_any(path, &policy.masked) {
                "masked"
            } else if matches_any(path, &policy.protected) {
                "protected"
            } else {
                return None;
            };
            Some(format!("path policy: {path} is {kind} and changed during the session"))
        })
        .collect()
}

fn matches_any(path: &str, entries: &[String]) -> bool {
    let components: Vec<&str> = path.split('/').collect();
    entries.iter().any(|entry| {
        let dir = entry.ends_with('/');
        let entry: Vec<&str> = entry.trim_end_matches('/').split('/').collect();
        // Component-for-component, never a string prefix: `.env` does not match `.envrc`, and
        // `.git/config` does not match `.git/config.bak`. A file entry is a suffix of the path; a
        // directory entry is a run of components anywhere in it.
        if dir {
            components.windows(entry.len()).any(|w| w == entry.as_slice())
        } else {
            components.ends_with(&entry)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn policy(masked: &[&str], protected: &[&str], unmasked: &[&str]) -> Policy {
        Policy {
            masked: masked.iter().map(|s| s.to_string()).collect(),
            protected: protected.iter().map(|s| s.to_string()).collect(),
            unmasked: unmasked.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The three path-list settings as their schema offers them, and a sandbox choice carrying
    /// settings for them.
    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "mask_paths": {"type": "array", "items": {"type": "string"}, "default": []},
                "protect_paths": {"type": "array", "items": {"type": "string"}, "default": []},
                "unmask_paths": {"type": "array", "items": {"type": "string"}, "default": []}
            }
        })
    }

    fn choice(settings: Value) -> ModuleChoice {
        ModuleChoice {
            provider: "microsandbox".into(),
            enabled: true,
            settings: serde_json::from_value(settings).unwrap(),
        }
    }

    #[test]
    fn defaults_mask_credentials_and_protect_the_agents_own_config() {
        let p = defaults();
        for (path, _) in DEFAULT_MASKED {
            assert!(p.masked.contains(&path.to_string()), "{path} is masked by default");
        }
        for (path, _) in DEFAULT_PROTECTED {
            let normalized = path.trim_end_matches('/');
            assert!(
                p.protected.iter().any(|e| e.trim_end_matches('/') == normalized),
                "{path} is protected by default"
            );
        }
        assert!(p.unmasked.is_empty(), "nothing is opted out by default");
    }

    #[test]
    fn settings_add_to_the_defaults_mask_wins_and_drop_unusable_entries() {
        let p = from_settings(
            &choice(json!({
                "mask_paths": ["secrets/credentials.json", "/etc/passwd", "../../etc", " .env ", "ok.env"],
                "protect_paths": [".env", "vendor/"],
                "unmask_paths": [".envrc", ".idea/"]
            })),
            &schema(),
        );
        // User entries join the built-ins, and mask wins: `.env` in both sets stays only masked.
        assert!(p.masked.contains(&"secrets/credentials.json".to_string()));
        assert!(p.protected.contains(&"vendor/".to_string()));
        assert!(p.masked.contains(&".env".to_string()));
        assert!(!p.protected.contains(&".env".to_string()));
        // Entries the gate refuses are dropped at resolution, never half-enforced: absolute
        // paths, traversal, and surrounding whitespace.
        assert!(p.masked.contains(&"ok.env".to_string()), "{p:?}");
        assert!(!p.masked.contains(&"/etc/passwd".to_string()), "absolute entries are dropped");
        assert!(!p.masked.contains(&"../../etc".to_string()), "traversal entries are dropped");
        assert!(!p.masked.contains(&" .env ".to_string()), "whitespace entries are dropped");
        // An opt-out clears the path from both sets, defaults included, and is logged with the
        // default's rationale.
        assert!(!p.masked.contains(&".envrc".to_string()));
        assert!(!p.protected.contains(&".idea/".to_string()));
        let note = opt_outs(&p).expect("the opt-out is logged");
        assert!(note.contains(".envrc") && note.contains("direnv exports"), "{note}");
        assert_eq!(opt_outs(&Policy::default()), None, "nothing opted out, nothing logged");
    }

    #[test]
    fn validation_rejects_roots_absolute_traversal_whitespace_and_separators() {
        for bad in [
            "", " ", "/", ".", "//", "a//b", "./a", "../a", "a/../b", "/abs", "a:b", "a,b", "a\nb", " .env", ".env ", "\t.env",
        ] {
            assert!(validate_path(bad).is_err(), "{bad:?} must be refused");
        }
        for good in [".env", "vendor/lib/.env", ".claude/", "a/b/c.txt", "a b"] {
            assert!(validate_path(good).is_ok(), "{good:?} must be accepted");
        }
        // And the one rule only a masked entry has: nothing under the git dir, which must stay
        // readable for the colony to boot and is already read-only anyway. Protecting it is fine.
        for bad in [".git", ".git/config", ".git/hooks/"] {
            assert!(validate_masked(bad).is_err(), "masking {bad:?} must be refused");
        }
        for good in [".git/config", ".git/hooks/", ".gitmodules"] {
            assert!(validate_path(good).is_ok(), "protecting {good:?} is allowed");
        }
    }

    #[test]
    fn violations_match_at_any_depth_whole_components_and_mask_wins() {
        // `.env` is in both sets and stays masked in the report, and a changed path that is a
        // symlink is reported whatever it is — what got staged is what reached the pull request.
        let p = policy(&[".env", ".npmrc"], &[".env", ".claude/", ".mcp.json"], &[]);
        let changed = [
            ".env",
            "vendor/lib/.env",
            ".envrc",
            ".claude/settings.json",
            "apps/x/.claude/settings.json",
            ".mcp.json",
            "src/main.rs",
            ".env/example",
            ".npmrc",
        ];
        let hits = violations(&changed.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &p);
        assert_eq!(hits.len(), 6, "{hits:?}");
        assert!(hits[0].contains(".env is masked"), "{hits:?}");
        assert!(hits[1].contains("vendor/lib/.env is masked"), "{hits:?}");
        assert!(hits[2].contains(".claude/settings.json is protected"), "{hits:?}");
        assert!(hits[3].contains("apps/x/.claude/settings.json is protected"), "{hits:?}");
        assert!(hits[4].contains(".mcp.json is protected"), "{hits:?}");
        assert!(hits[5].contains(".npmrc is masked"), "{hits:?}");
    }

    #[test]
    fn nested_git_entries_match_their_protected_defaults() {
        let p = policy(&[], &[".git/config"], &[]);
        assert_eq!(violations(&["vendor/lib/.git/config".to_string()], &p).len(), 1);
        assert!(violations(&[".git/config.bak".to_string()], &p).is_empty());
    }

    /// The regression the round trip exists for: trailing slashes survive the policy file, so a
    /// directory entry keeps matching the files below it once publish rebuilds the policy from the
    /// bind list the guest consumed.
    #[test]
    fn directory_semantics_survive_the_policy_file_round_trip() {
        let p = policy(&[".env", ".secrets/"], &[".claude/", ".mcp.json"], &[]);
        let wt = tempfile("round_trip");
        let planned = plan(&wt.path, &p, &[]).unwrap();
        assert_eq!(
            planned.binds,
            vec![
                "mask-file .env",
                "mask-dir .secrets/",
                "protect .claude/",
                "protect .mcp.json"
            ],
            "{planned:?}"
        );
        let back = from_bind_list(&planned.binds.join("\n"));
        assert_eq!(back.masked, vec![".env".to_string(), ".secrets/".to_string()], "{back:?}");
        assert_eq!(back.protected, vec![".claude/".to_string(), ".mcp.json".to_string()]);
        let hits = violations(
            &[".claude/settings.json".to_string(), "vendor/x/.claude/a".to_string()],
            &back,
        );
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits.iter().all(|h| h.contains("is protected")), "{hits:?}");
        wt.close();
    }

    #[test]
    fn plan_and_apply_create_placeholders_and_leave_existing_files_alone() {
        let p = policy(&[".env", "secrets/keys.json", ".cache"], &[], &[]);
        let wt = tempfile("plan_and_apply");
        // An existing file keeps its content; an absent file and an absent directory are planned
        // as placeholders; the kind follows what is on disk, not how the entry was spelled.
        std::fs::write(wt.path.join(".cache"), "keepme").unwrap();
        let planned = plan(&wt.path, &p, &[]).unwrap();
        assert_eq!(
            planned.binds,
            vec!["mask-file .env", "mask-file secrets/keys.json", "mask-file .cache"],
            "{planned:?}"
        );
        assert_eq!(
            planned.placeholders,
            vec![(".env".to_string(), false), ("secrets/keys.json".to_string(), false)]
        );
        // A plan is a plan: nothing exists until apply runs.
        assert!(!wt.path.join(".env").exists(), "plan must not create");
        assert!(apply(&wt.path, &planned).is_ok());
        assert_eq!(std::fs::read(wt.path.join(".env")).unwrap(), b"");
        assert_eq!(std::fs::read(wt.path.join("secrets/keys.json")).unwrap(), b"");
        assert!(wt.path.join("secrets").is_dir(), "apply creates missing parents");
        assert!(wt.path.join(".cache").is_file(), "the existing path was not touched");
        assert_eq!(std::fs::read(wt.path.join(".cache")).unwrap(), b"keepme");
        let again = plan(&wt.path, &p, &[]).unwrap();
        assert!(again.placeholders.is_empty(), "a second plan re-reports nothing: {again:?}");
        wt.close();
    }

    /// The host resolves symlinks while the VM is down, so the guest never has to follow one: a
    /// link that stays inside the checkout is bound at its target; one that leaves it, dangles, or
    /// sits in an entry's parent chain is skipped and named on the boot log, nothing bound or
    /// created through it.
    #[test]
    fn a_symlink_is_resolved_to_its_in_worktree_target_or_skipped() {
        let p = policy(&[".env", ".envrc", ".pypirc", "link/target.env"], &[".mcp.json"], &[]);
        let wt = tempfile("symlink");
        std::fs::create_dir(wt.path.join("config")).unwrap();
        std::fs::write(wt.path.join("config/prod.env"), "SECRET=1").unwrap();
        std::fs::write(wt.path.join("config/mcp.json"), "{}").unwrap();
        std::fs::create_dir(wt.path.join("real")).unwrap();
        symlink("config/prod.env", wt.path.join(".env"));
        symlink("/etc/passwd", wt.path.join(".envrc"));
        symlink("nowhere", wt.path.join(".pypirc"));
        symlink(wt.path.join("real"), wt.path.join("link"));
        symlink("config/mcp.json", wt.path.join(".mcp.json"));
        let planned = plan(&wt.path, &p, &[]).unwrap();
        assert_eq!(
            planned.binds,
            vec!["mask-file config/prod.env", "protect config/mcp.json"],
            "{planned:?}"
        );
        assert!(planned.placeholders.is_empty(), "{planned:?}");
        assert_eq!(
            planned.skipped,
            vec![".envrc".to_string(), ".pypirc".to_string(), "link/target.env".to_string()]
        );
        assert!(wt.path.join("config/prod.env").is_file(), "nothing was created or removed");
        assert!(!wt.path.join("real/target.env").exists(), "nothing created through the link");
        // The skips reach the boot's summary line, which stays quiet when there is nothing to say.
        let line = summary(&p, &planned);
        assert!(
            line.contains("skipped") && line.contains(".envrc") && line.contains(".pypirc"),
            "{line}"
        );
        assert!(!summary(&p, &Materialized::default()).contains("skipped"));
        wt.close();
    }

    /// A resume keeps the earlier boot's still-empty placeholders on the books without re-claiming
    /// files it never created, and publish's clean removes exactly those — a masked placeholder
    /// cannot be filled through its `/dev/null` bind, so "still empty" means "never real work" —
    /// while a filled directory and a missing entry are left as they are.
    #[test]
    fn a_previous_placeholder_stays_on_the_books_and_clean_takes_still_empty_only() {
        let p = policy(&[".env"], &[".claude/"], &[]);
        let wt = tempfile("clean_placeholders");
        let planned = plan(&wt.path, &p, &[]).unwrap();
        assert_eq!(
            planned.placeholders,
            vec![(".env".to_string(), false), (".claude/".to_string(), true)]
        );
        apply(&wt.path, &planned).unwrap();
        // A resume boots over the same worktree: the still-empty `.env` is kept on the books from
        // the earlier boot's list for publish to remove, while the `.claude` the colony filled —
        // through a path the policy let it reach — is no longer "still empty" and drops off.
        std::fs::write(wt.path.join(".claude/settings.json"), "{}").unwrap();
        let resumed = plan(&wt.path, &p, &planned.placeholder_names()).unwrap();
        assert_eq!(resumed.placeholders, vec![(".env".to_string(), false)], "{resumed:?}");
        let mut list = planned.placeholder_names();
        list.push(".missing".to_string());
        let removed = clean_placeholders(&wt.path, &fake_list(&wt, &list)).unwrap();
        assert_eq!(removed, vec![".env".to_string()], "{removed:?}");
        assert!(!wt.path.join(".env").exists(), "the still-empty placeholder is gone");
        assert!(wt.path.join(".claude/settings.json").exists(), "a filled directory stays");
        wt.close();
    }

    /// The second line of defence at publish: an empty untracked path at a policy entry goes even
    /// without a placeholder list, and nothing tracked, filled, linked or missing is touched.
    #[test]
    fn the_untracked_safety_net_removes_only_empty_untracked_paths() {
        let wt = tempfile("safety_net");
        std::fs::write(wt.path.join("tracked.env"), "").unwrap();
        std::fs::write(wt.path.join("empty.env"), "").unwrap();
        std::fs::write(wt.path.join("filled.env"), "S=1").unwrap();
        std::fs::create_dir(wt.path.join("emptydir")).unwrap();
        std::fs::create_dir(wt.path.join("fulldir")).unwrap();
        std::fs::write(wt.path.join("fulldir/x"), "1").unwrap();
        symlink("filled.env", wt.path.join("linked.env"));
        let paths = [
            "tracked.env",
            "empty.env",
            "filled.env",
            "emptydir",
            "fulldir",
            "linked.env",
            "missing",
        ];
        let removed = remove_empty_untracked(
            &wt.path,
            &paths.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &["tracked.env".to_string()],
        );
        assert_eq!(removed, vec!["empty.env".to_string(), "emptydir".to_string()], "{removed:?}");
        assert!(wt.path.join("tracked.env").exists(), "tracked is never deleted");
        assert_eq!(std::fs::read(wt.path.join("filled.env")).unwrap(), b"S=1");
        assert!(wt.path.join("fulldir/x").exists(), "a filled directory stays");
        assert!(wt.path.join("linked.env").exists(), "a symlink is never followed or removed");
        wt.close();
    }

    /// Writes a stand-in placeholder list inside the worktree, the way a boot would have left one
    /// in `vm/`; the file is removed with the worktree by `close()`.
    fn fake_list(wt: &TempDirGuard, placeholder_list: &[String]) -> PathBuf {
        let list = wt.path.join("placeholders-list");
        std::fs::write(&list, placeholder_list.join("\n")).unwrap();
        list
    }

    /// Little local tempdir helper, so the tests do not need a new dev-dependency. Named and
    /// counter-stamped, because cargo runs tests in parallel threads of one process; `close`
    /// removes the tree.
    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn close(self) {
            std::fs::remove_dir_all(&self.path).ok();
        }
    }

    fn tempfile(name: &str) -> TempDirGuard {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "colonizer-path-policy-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDirGuard { path: dir }
    }

    #[cfg(unix)]
    fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }
}
