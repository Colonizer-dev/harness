//! Enforcing the path policy on paths that appear after boot (issue #648). The boot binds the
//! policy for the paths that existed then; a checkout that appears mid-session — a clone, a
//! `git init`, a worktree or a submodule, any directory below the workspace with its own `.git` —
//! got nothing until publish named the changed paths. So agentd watches the workspace: every
//! directory strictly below it that carries a `.git` entry is a checkout root, and each policy
//! path is bound relative to that root as it appears, with the mounts the boot script applies
//! (docs/path-policy.md). agentd runs unfiltered in the VM's mount namespace — only runner
//! children lose `mount` (harden.rs) — so its binds stick.
//!
//! Every target is pinned before it is touched: openat2 with `RESOLVE_NO_SYMLINKS` and
//! `RESOLVE_BENEATH` against the held-open workspace, then mounted through `/proc/self/fd`, so
//! the kernel lays the bind on the inode that was checked and never re-resolves a path an agent
//! may be racing. Best effort by design: a failed mount, or a path that cannot be pinned safely,
//! is one warn event and never a stopped daemon — unlike the boot, which fails closed, a
//! mid-session miss is still reported at publish. A read that races the watcher can see a
//! just-created masked file.

use std::{path::Path, path::PathBuf, sync::Arc};

use crate::store::{EventStore, log_event};
#[cfg(target_os = "linux")]
use std::{
    collections::HashMap,
    ffi::{CString, OsString},
    io,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{ffi::OsStringExt, io::AsRawFd},
    },
    time::Instant,
};
use std::{collections::HashSet, time::Duration};

/// How often the workspace is re-walked, whatever the event stream is doing: the backstop for a
/// watch that could not be added, a queue overflow, and anything else the event stream missed.
#[cfg(target_os = "linux")]
const RESCAN: Duration = Duration::from_secs(30);

/// The cap on the skip-and-failure keys remembered, so no tree can grow them without end.
const QUIET: usize = 256;

/// One enforcement action from the policy file — `mask-file <path>`, `mask-dir <path/>` or
/// `protect <path>` — the path worktree-relative, a directory entry keeping its trailing `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    kind: Kind,
    rel: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    MaskFile,
    MaskDir,
    Protect,
}

impl Pattern {
    fn new(kind: Kind, rel: impl Into<String>) -> Self {
        Self { kind, rel: rel.into() }
    }

    /// The entry without its trailing `/`.
    fn bare(&self) -> &str {
        self.rel.trim_end_matches('/')
    }

    /// Whether the bind goes over a directory: a `mask-dir`, or a `protect` spelled with `/`.
    fn want_dir(&self) -> bool {
        matches!(self.kind, Kind::MaskDir) || self.rel.ends_with('/')
    }

    /// The path this pattern binds inside a checkout root.
    fn target(&self, root: &Path) -> PathBuf {
        root.join(self.bare())
    }

    /// The verb for the event line, the boot's and publish's wording.
    fn verb(&self) -> &'static str {
        match self.kind {
            Kind::MaskFile | Kind::MaskDir => "masked",
            Kind::Protect => "protected",
        }
    }
}

/// The policy file's actions, in order, plus the two protected `.git` entries the boot never
/// binds: at the workspace root the git admin dir is host-mounted read-only instead, but a nested
/// checkout's `.git` is a plain directory, and the policy's named entries (`.git/config`,
/// `.git/hooks/`) cover it. Masked `.git` entries cannot occur — the save-time gate refuses them.
fn patterns(text: &str) -> Vec<Pattern> {
    let mut out: Vec<Pattern> = text
        .lines()
        // Split the way the boot script's `read -r kind rel` does: one whitespace, then the rest.
        .filter_map(|line| {
            let (kind, rel) = line.split_once(char::is_whitespace)?;
            Some((kind, rel.trim()))
        })
        .filter_map(|(kind, rel)| {
            let kind = match kind {
                "mask-file" => Kind::MaskFile,
                "mask-dir" => Kind::MaskDir,
                "protect" => Kind::Protect,
                _ => return None,
            };
            // The boot writes worktree-relative paths; an absolute or traversing one would send a
            // bind out of the checkout. Dropped, not fatal — the boot's own loop already failed
            // closed on a policy it could not carry, before this daemon ever ran.
            let usable = !rel.is_empty()
                && !rel.starts_with('/')
                && !rel.trim_end_matches('/').split('/').any(|c| matches!(c, "" | "." | ".."));
            usable.then_some(Pattern::new(kind, rel))
        })
        .collect();
    for rel in [".git/config", ".git/hooks/"] {
        if !out.iter().any(|p| p.rel == rel) {
            out.push(Pattern::new(Kind::Protect, rel));
        }
    }
    out
}

/// An `O_PATH` descriptor for `rel` under `dir`, refusing every symlink (`RESOLVE_NO_SYMLINKS`)
/// and anything that escapes `dir` (`RESOLVE_BENEATH`): what the caller stats and mounts is the
/// inode behind the name at pin time, never a path re-resolved after the check.
#[cfg(target_os = "linux")]
fn pin(dir: i32, rel: &std::ffi::OsStr) -> io::Result<OwnedFd> {
    let c = CString::new(rel.as_encoded_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut how: libc::open_how = unsafe { std::mem::zeroed() }; // all-`u64` fields: zeroed is valid
    how.flags = (libc::O_PATH | libc::O_CLOEXEC) as u64;
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS;
    // SAFETY: a directory descriptor, a NUL-terminated relative path and the flags struct; the
    // kernel keeps nothing it is handed.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir,
            c.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: ownership of the descriptor the syscall just handed out.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as _) })
    }
}

/// The deepest checkout root that contains `dir` (or is it): where an event's path gets its
/// policy from.
#[cfg(target_os = "linux")]
fn enclosing_root(roots: &HashSet<PathBuf>, dir: &Path) -> Option<PathBuf> {
    roots
        .iter()
        .filter(|root| dir.starts_with(*root))
        .max_by_key(|root| root.as_os_str().len())
        .cloned()
}

/// Reads the policy and starts the watcher thread; a no-op without a policy file, which is how
/// agentd always runs outside a colony VM (tests, `--exec-hardened`).
#[cfg(target_os = "linux")]
pub(crate) fn start(workspace: &Path, policy: &Path, store: Arc<EventStore>) {
    let Ok(text) = std::fs::read_to_string(policy) else { return };
    let patterns = patterns(&text);
    if patterns.is_empty() {
        return;
    }
    let warn = |message: String| store.append(log_event("warn", message));
    // Resolved absolute, so the paths compared against /proc/self/mountinfo — what is bound
    // already, and whether a protect took — spell the mount points the way the kernel does, even
    // when the config named the workspace through a symlink.
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let Ok(workspace_fd) = std::fs::File::open(&workspace) else {
        warn(format!(
            "path policy: cannot open the workspace `{}`; nothing watched",
            workspace.display()
        ));
        return;
    };
    // SAFETY: plain descriptor creation; the kernel is handed flags, nothing else. A failure is a
    // warn and a watcher with no events to lose: the periodic rescan is all there is.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    if fd < 0 {
        warn(format!(
            "path policy: inotify is unavailable: {}; the periodic rescan covers it",
            io::Error::last_os_error()
        ));
    }
    let watcher = Watcher {
        workspace,
        workspace_fd,
        patterns,
        store,
        roots: HashSet::new(),
        quiet: HashSet::new(),
        misses: (0, None),
        fd,
        watched: HashMap::new(),
        watches: HashMap::new(),
    };
    if std::thread::Builder::new()
        .name("path-policy".into())
        .spawn(move || watcher.run())
        .is_err()
    {
        eprintln!("colonizer-agentd: warning: cannot start the path-policy watcher");
    }
}

/// How often the off-Linux poller re-walks the workspace.
#[cfg(not(target_os = "linux"))]
const POLL: Duration = Duration::from_secs(1);

/// How deep below the workspace the off-Linux poller looks for a nested checkout's `.git`.
#[cfg(not(target_os = "linux"))]
const POLL_DEPTH: usize = 8;

/// The most directory entries one off-Linux poll reads, so a huge tree costs a bounded walk.
#[cfg(not(target_os = "linux"))]
const POLL_BUDGET: usize = 20_000;

/// Off Linux (macOS dev builds of the workspace) there is no bind mount and no inotify, so nothing
/// can be enforced — but a nested checkout that appears mid-session must not pass silently. A
/// thread polls the workspace every [`POLL`], bounded in depth and entries, for directories with
/// their own `.git`, and emits one warn per policy path found in each that it cannot bind: the
/// same `cannot apply` line the Linux watcher writes when a mount fails. Each path is reported
/// once while it exists.
#[cfg(not(target_os = "linux"))]
pub(crate) fn start(workspace: &Path, policy: &Path, store: Arc<EventStore>) {
    let Ok(text) = std::fs::read_to_string(policy) else { return };
    let patterns = patterns(&text);
    if patterns.is_empty() {
        return;
    }
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let poller = Poller {
        workspace,
        patterns,
        store,
        reported: HashSet::new(),
        budget_warned: false,
    };
    if std::thread::Builder::new()
        .name("path-policy".into())
        .spawn(move || poller.run())
        .is_err()
    {
        eprintln!("colonizer-agentd: warning: cannot start the path-policy poller");
    }
}

#[cfg(not(target_os = "linux"))]
struct Poller {
    workspace: PathBuf,
    patterns: Vec<Pattern>,
    store: Arc<EventStore>,
    /// Targets already reported, dropped once they are gone so a re-created one is named again;
    /// capped at [`QUIET`].
    reported: HashSet<PathBuf>,
    budget_warned: bool,
}

#[cfg(not(target_os = "linux"))]
impl Poller {
    fn run(mut self) {
        loop {
            self.reported.retain(|target| std::fs::symlink_metadata(target).is_ok());
            let mut roots = Vec::new();
            let mut budget = POLL_BUDGET;
            let workspace = self.workspace.clone();
            walk(&workspace, 0, &mut budget, &mut roots);
            if budget == 0 && !self.budget_warned {
                self.budget_warned = true;
                let say = format!(
                    "path policy: the workspace has more than {POLL_BUDGET} entries; nested checkouts past them are not reported"
                );
                self.store.append(log_event("warn", say));
            }
            for root in roots {
                self.report(&root);
            }
            std::thread::sleep(POLL);
        }
    }

    /// One warn per policy path present in `root` in the kind its bind takes, not yet reported.
    fn report(&mut self, root: &Path) {
        for pattern in &self.patterns {
            let target = pattern.target(root);
            if self.reported.contains(&target) {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&target) else {
                continue;
            };
            let why = if meta.file_type().is_symlink() {
                "a symlink is never bound"
            } else if meta.is_dir() != pattern.want_dir() {
                continue; // a kind the bind would not take — the Linux watcher skips it too
            } else {
                "bind mounts need Linux; nothing bound"
            };
            if self.reported.len() >= QUIET {
                return;
            }
            let say = format!(
                "path policy: cannot apply {} `{}`: {why} (nested checkout `{}`)",
                pattern.verb(),
                relative(&self.workspace, &target),
                relative(&self.workspace, root)
            );
            self.store.append(log_event("warn", say));
            self.reported.insert(target);
        }
    }
}

/// Collects every directory strictly below the workspace (depth > 0) that carries a `.git` entry,
/// never following a symlink, never entering a `.git`, at most [`POLL_DEPTH`] deep and reading at
/// most `budget` entries in all.
#[cfg(not(target_os = "linux"))]
fn walk(dir: &Path, depth: usize, budget: &mut usize, roots: &mut Vec<PathBuf>) {
    if depth > 0 && std::fs::symlink_metadata(dir.join(".git")).is_ok() {
        roots.push(dir.to_path_buf());
    }
    if depth >= POLL_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let child = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) && entry.file_name() != ".git" {
            walk(&child, depth + 1, budget, roots);
        }
    }
}

/// A workspace-relative spelling of `path`, for the event lines.
#[cfg(not(target_os = "linux"))]
fn relative(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace).unwrap_or(path).to_string_lossy().into_owned()
}

#[cfg(target_os = "linux")]
struct Watcher {
    workspace: PathBuf,
    /// The workspace held open: every target is pinned relative to this descriptor, so nothing
    /// below it is ever re-resolved by name once checked.
    workspace_fd: std::fs::File,
    patterns: Vec<Pattern>,
    store: Arc<EventStore>,
    /// Checkout roots found so far: directories strictly below the workspace with a `.git` entry.
    roots: HashSet<PathBuf>,
    /// Skips and failures already reported, so a rescan logs no second line.
    quiet: HashSet<String>,
    /// Watch-add failures since the last line: the count and the first directory, one warn total.
    misses: (u32, Option<PathBuf>),
    fd: i32,
    /// The directories under watch, both ways round: by path so a rescan re-adds nothing, by
    /// descriptor so an event finds its directory.
    watched: HashMap<PathBuf, i32>,
    watches: HashMap<i32, PathBuf>,
}

#[cfg(target_os = "linux")]
impl Watcher {
    fn run(mut self) {
        loop {
            // One warn per burst of watch failures, naming the count and the first directory: a
            // tree that exhausts the watch quota must not flood the colony log.
            if let Some(first) = self.misses.1.take() {
                let count = std::mem::take(&mut self.misses.0);
                let say = format!(
                    "path policy: could not watch {count} directories, `{}` first; the periodic rescan covers them",
                    self.relative(&first)
                );
                self.store.append(log_event("warn", say));
            }
            self.prune();
            let mut mounts: HashSet<PathBuf> = mountinfo().into_iter().map(|(point, _)| point).collect();
            let workspace = self.workspace.clone();
            self.scan(&workspace, &mut mounts);
            let since = Instant::now();
            // Precisely handled event batches need no new walk; the elapsed interval and the
            // timeouts and overflows do.
            while !self.wait_for_events(since) {}
        }
    }

    /// Drops the state a full walk would otherwise carry forever: checkout roots whose `.git`
    /// went away, and watches on directories that are gone.
    fn prune(&mut self) {
        self.roots.retain(|root| std::fs::symlink_metadata(root.join(".git")).is_ok());
        for (wd, dir) in std::mem::take(&mut self.watches) {
            if std::fs::symlink_metadata(&dir).is_ok() {
                self.watches.insert(wd, dir);
                continue;
            }
            self.watched.remove(&dir);
            if self.fd >= 0 {
                // SAFETY: a descriptor the kernel handed out; a stale one answers EINVAL, ignored.
                unsafe { libc::inotify_rm_watch(self.fd, wd) };
            }
        }
    }

    /// Walks `dir` and everything below it — never into a `.git` directory, never into an
    /// already-masked one, whose contents are hidden anyway. Every directory gets a watch, every
    /// checkout root gets claimed, and each root is enforced after its subtree has been walked, so
    /// binds and watches are all in place before the loop waits again. The workspace root itself
    /// is not a checkout root: its paths were the boot's business.
    fn scan(&mut self, dir: &Path, mounts: &mut HashSet<PathBuf>) {
        self.watch(dir);
        let git = dir.join(".git");
        let is_root = dir != self.workspace && std::fs::symlink_metadata(&git).is_ok();
        if is_root {
            self.roots.insert(dir.to_path_buf());
            self.watch(&git); // for the protected `.git/config` and `.git/hooks/`; never walked
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let child = entry.path();
                // DirEntry::file_type never follows symlinks; only real directories are walked.
                let walk = entry.file_type().is_ok_and(|t| t.is_dir())
                    && child.file_name().is_some_and(|n| n.as_encoded_bytes() != b".git")
                    && !mounts.contains(&child);
                if walk {
                    self.scan(&child, mounts);
                }
            }
        }
        if is_root {
            self.apply(dir, None, mounts);
        }
    }

    /// Blocks for events or until the walk is due — [`RESCAN`] after the last one, whatever the
    /// event stream is doing — and handles one batch precisely. Answers whether the caller should
    /// do a full rescan: a due walk, a quiet period, an overflow, a lost read.
    fn wait_for_events(&mut self, since: Instant) -> bool {
        if self.fd < 0 {
            std::thread::sleep(RESCAN); // no inotify: the periodic walk is all there is
            return true;
        }
        let due = RESCAN.saturating_sub(since.elapsed());
        if due.is_zero() {
            return true; // a steady event stream must not starve the backstop walk
        }
        let mut fds = [libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one pollfd for a descriptor this thread owns.
        if unsafe { libc::poll(fds.as_mut_ptr(), 1, due.as_millis() as i32) } <= 0 {
            return true; // the timeout (or a signal): the backstop walk
        }
        let mut buf = [0u8; 64 * 1024];
        // SAFETY: the descriptor is this thread's and the buffer outlives the call; whatever did
        // not fit stays queued for the next read.
        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return true; // a raced EAGAIN or an error: the walk re-checks everything
        }
        let mut mounts: HashSet<PathBuf> = mountinfo().into_iter().map(|(point, _)| point).collect();
        let (mut off, mut overflow, n) = (0, false, n as usize);
        while off + 16 <= n {
            // struct inotify_event: i32 wd, u32 mask, u32 cookie, u32 len, then the name.
            let wd = i32::from_ne_bytes(buf[off..off + 4].try_into().unwrap());
            let mask = u32::from_ne_bytes(buf[off + 4..off + 8].try_into().unwrap());
            let len = u32::from_ne_bytes(buf[off + 12..off + 16].try_into().unwrap()) as usize;
            let name = &buf[off + 16..(off + 16 + len).min(n)];
            off += 16 + len;
            if mask & libc::IN_Q_OVERFLOW != 0 {
                overflow = true; // events were dropped; only the walk can say what is there now
            } else {
                self.handle(&mut mounts, wd, mask, name);
            }
        }
        overflow || since.elapsed() >= RESCAN // a batch that outlasted the interval ends in the walk
    }

    /// One event: a directory gets watched and walked (it may carry a checkout), a `.git` makes
    /// its parent a checkout root, and anything that appears is enforced at its root.
    fn handle(&mut self, mounts: &mut HashSet<PathBuf>, wd: i32, mask: u32, name: &[u8]) {
        if mask & libc::IN_IGNORED != 0 {
            if let Some(dir) = self.watches.remove(&wd) {
                self.watched.remove(&dir); // the directory went away; its watch number may come back
            }
            return;
        }
        let Some(dir) = self.watches.get(&wd).cloned() else { return };
        let name = OsString::from_vec(name.split(|b| *b == 0).next().unwrap_or_default().to_vec());
        if name.is_empty() {
            return;
        }
        if name.as_encoded_bytes() == b".git" && dir != self.workspace {
            // Its parent just became a checkout root: the scan claims it, binds whatever the
            // checkout carries, and watches its `.git` — without ever walking it.
            self.scan(&dir, mounts);
            return;
        }
        if mask & libc::IN_ISDIR != 0 && dir.file_name().is_some_and(|n| n.as_encoded_bytes() != b".git") {
            let fresh = dir.join(&name);
            self.scan(&fresh, mounts); // a new or moved-in tree can carry a whole checkout
        }
        if let Some(root) = enclosing_root(&self.roots, &dir) {
            self.apply(&root, Some(&dir.join(&name)), mounts);
        }
    }

    /// Adds the create/move watch for one directory — once, for the watch is remembered by path,
    /// so a rescan re-adds nothing.
    fn watch(&mut self, dir: &Path) {
        if self.watched.contains_key(dir) {
            return;
        }
        let Ok(c) = CString::new(dir.as_os_str().as_encoded_bytes()) else {
            return;
        };
        // SAFETY: the path is a NUL-terminated C string; the descriptor is this thread's.
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), libc::IN_CREATE | libc::IN_MOVED_TO) };
        if wd < 0 {
            self.misses.0 += 1; // one aggregated warn per burst, reported before the next walk
            if self.misses.1.is_none() {
                self.misses.1 = Some(dir.to_path_buf());
            }
        } else {
            self.watched.insert(dir.to_path_buf(), wd);
            self.watches.insert(wd, dir.to_path_buf());
        }
    }

    /// Enforces the policy inside one checkout root: every pattern, or — after an event — just
    /// the one naming `at`, the cheap route a busy tree pays near nothing per event. Each target
    /// is pinned first, so the bind lands on the inode the kind check saw.
    fn apply(&mut self, root: &Path, at: Option<&Path>, mounts: &mut HashSet<PathBuf>) {
        for index in 0..self.patterns.len() {
            let target = self.patterns[index].target(root);
            if at.is_some_and(|path| target != path) || mounts.contains(&target) {
                continue; // not this event's path, or already bound — what keeps repeats idempotent
            }
            let pattern = self.patterns[index].clone();
            let rel = target.strip_prefix(&self.workspace).unwrap_or(&target);
            let fd = match pin(self.workspace_fd.as_raw_fd(), rel.as_os_str()) {
                // The common miss: the target is not there (yet), or a file stands in the path.
                Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => continue,
                Err(e) => {
                    // A symlink anywhere in the way answers ELOOP: skipped with its line, the
                    // boot refuses a symlink outright; anything else is a warn, never a crash.
                    let say = if e.raw_os_error() == Some(libc::ELOOP) {
                        format!("path policy: `{}` is a symlink; nothing bound", self.relative(&target))
                    } else {
                        format!("path policy: cannot pin `{}`: {e}", self.relative(&target))
                    };
                    self.once(format!("pin {}", target.display()), say);
                    continue;
                }
                Ok(fd) => fd,
            };
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: the descriptor is owned by `fd` above and the buffer outlives the call.
            let ok = unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } == 0;
            if !ok || (stat.st_mode & libc::S_IFMT == libc::S_IFDIR) != pattern.want_dir() {
                continue; // unreadable, or a kind the bind would not take — the boot's own checks
            }
            let name = self.relative(&target);
            if let Err(e) = apply_mount(pattern.kind, fd.as_raw_fd()) {
                let say = format!("path policy: cannot apply {} `{}`: {e}", pattern.verb(), name);
                self.once(format!("bind {}", target.display()), say);
                continue;
            }
            mounts.insert(target.clone()); // this batch's own binds count as taken, like any other
            // A protected path's read-only remount needs a pin taken *after* the bind: the
            // descriptor above still names the underlying mount, and a remount through it is
            // refused or lands under the bind. So re-pin — a fresh walk crosses into the new
            // bind — and require the inode we bound (a bind keeps the superblock, so st_dev
            // matches too) before the flags go on.
            let mut verified = pattern.kind != Kind::Protect;
            if !verified && let Ok(again) = pin(self.workspace_fd.as_raw_fd(), rel.as_os_str()) {
                let mut now: libc::stat = unsafe { std::mem::zeroed() };
                // SAFETY: the descriptor `again` owns and the buffer above.
                if unsafe { libc::fstat(again.as_raw_fd(), &mut now) } == 0
                    && now.st_dev == stat.st_dev
                    && now.st_ino == stat.st_ino
                    && remount_ro(again.as_raw_fd()).is_ok()
                {
                    verified = mount_is_ro(&target);
                }
            }
            if !verified {
                // A bind that lost its read-only remount is half a protection.
                self.once(
                    format!("ro {}", target.display()),
                    format!("path policy: `{name}` is bound but not read-only"),
                );
                continue;
            }
            let say = format!(
                "path policy: {} `{name}` (nested checkout `{}`)",
                pattern.verb(),
                self.relative(root)
            );
            self.store.append(log_event("info", say));
        }
    }

    /// A workspace-relative spelling of `path`, for the event lines.
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    /// Appends one warn event per distinct key, so rescans and repeats stay one line each — no
    /// more than [`QUIET`] keys, so no tree can grow the set without end.
    fn once(&mut self, key: String, message: String) {
        if (self.quiet.len() < QUIET || self.quiet.contains(&key)) && self.quiet.insert(key) {
            self.store.append(log_event("warn", message));
        }
    }
}

/// One `mount(2)` call, `Ok` on success, the errno as an error otherwise.
#[cfg(target_os = "linux")]
fn mount(
    source: *const std::ffi::c_char,
    target: *const std::ffi::c_char,
    fstype: *const std::ffi::c_char,
    flags: libc::c_ulong,
    data: *const std::ffi::c_void,
) -> io::Result<()> {
    // SAFETY: every argument is a NUL-terminated C string or null; mount(2) keeps nothing it gets.
    if unsafe { libc::mount(source, target, fstype, flags, data) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The mount the boot script applies for `kind`, laid on `/proc/self/fd/{fd}` — the descriptor
/// pins the inode that was checked, so the kernel never re-resolves the path an agent may be
/// racing. A masked file is covered by a bind of `/dev/null`, a masked directory by an empty
/// read-only tmpfs of the same explicit tiny size, a protected path by a self-bind of itself,
/// which [`remount_ro`] makes read-only afterwards.
#[cfg(target_os = "linux")]
fn apply_mount(kind: Kind, fd: i32) -> io::Result<()> {
    let path = CString::new(format!("/proc/self/fd/{fd}"))?;
    let (p, none, size) = (path.as_ptr(), std::ptr::null::<std::ffi::c_char>(), c"size=4k,mode=0555");
    match kind {
        Kind::MaskFile => mount(c"/dev/null".as_ptr(), p, none, libc::MS_BIND, none.cast()),
        Kind::MaskDir => mount(
            c"colonizer-mask".as_ptr(),
            p,
            c"tmpfs".as_ptr(),
            libc::MS_RDONLY,
            size.as_ptr().cast(),
        ),
        Kind::Protect => mount(p, p, none, libc::MS_BIND, none.cast()), // a self-bind of the pinned inode
    }
}

/// The read-only remount a protected path takes after its self-bind, through `/proc/self/fd/{fd}`
/// of a descriptor pinned *after* the bind — the first pin still names the underlying mount, and
/// a remount through it is refused or lands under the bind (see `apply`).
#[cfg(target_os = "linux")]
fn remount_ro(fd: i32) -> io::Result<()> {
    let path = CString::new(format!("/proc/self/fd/{fd}"))?;
    mount(
        std::ptr::null(),
        path.as_ptr(),
        std::ptr::null(),
        libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
        std::ptr::null(),
    )
}

/// A mountinfo field with the kernel's octal escapes (`\040` and friends) decoded.
#[cfg(target_os = "linux")]
fn unescaped(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut rest = field.iter();
    while let Some(b) = rest.next() {
        if *b != b'\\' {
            out.push(*b);
            continue;
        }
        let digits: Vec<_> = rest.by_ref().take(3).collect();
        if digits.len() == 3 && digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
            out.push(((digits[0] - b'0') << 6) | ((digits[1] - b'0') << 3) | (digits[2] - b'0'));
        } else {
            out.push(*b);
            out.extend(digits);
        }
    }
    out
}

/// Every mount in this namespace as its mount point and its options, from `/proc/self/mountinfo`:
/// a point already in the set is a bound target, which keeps repeated passes from stacking mounts,
/// and the options are how a protected path's read-only remount is verified.
#[cfg(target_os = "linux")]
fn mountinfo() -> Vec<(PathBuf, String)> {
    let Ok(bytes) = std::fs::read("/proc/self/mountinfo") else {
        return Vec::new();
    };
    bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| {
            let mut fields = line.split(|b| *b == b' ');
            let point = PathBuf::from(OsString::from_vec(unescaped(fields.nth(4)?)));
            let options = String::from_utf8_lossy(fields.next()?).into_owned();
            Some((point, options))
        })
        .collect()
}

/// Whether the mount at `point` carries its read-only flag, read back from the kernel — how a
/// protected path's remount is verified. (seal.rs reads its bind back; a protected path's content
/// is unchanged, so its mount options are the tell.)
#[cfg(target_os = "linux")]
fn mount_is_ro(point: &Path) -> bool {
    mountinfo()
        .into_iter()
        .any(|(at, options)| at == point && options.split(',').any(|option| option == "ro"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boot's bind list is what the watcher polices by: `kind path` lines, directory entries
    /// with their trailing `/`, and nothing else — noise and unusable paths are dropped.
    #[test]
    fn the_policy_file_parses_into_kinds_and_directory_entries_keep_their_slash() {
        let ps = patterns(
            "mask-file .env\nmask-dir .secrets/\nprotect .mcp.json\nprotect .claude/\n\nnoise x\nmask-file /abs\nmask-file ../up\nprotect a//b\n",
        );
        assert_eq!(
            ps[..4],
            [
                Pattern::new(Kind::MaskFile, ".env"),
                Pattern::new(Kind::MaskDir, ".secrets/"),
                Pattern::new(Kind::Protect, ".mcp.json"),
                Pattern::new(Kind::Protect, ".claude/"),
            ]
        );
        assert_eq!(ps[0].target(Path::new("/ws/vendor/lib")), Path::new("/ws/vendor/lib/.env"));
        assert!(ps[1].want_dir() && !ps[2].want_dir() && ps[3].want_dir());
        assert_eq!((ps[0].verb(), ps[2].verb()), ("masked", "protected"));
        // And the two `.git` entries the boot enforces with its own mount are the watcher's to
        // carry for nested checkouts, appended once.
        assert_eq!(ps[4], Pattern::new(Kind::Protect, ".git/config"));
        assert_eq!(ps.last().map(|p| p.rel.as_str()), Some(".git/hooks/"));
    }

    /// An event is policed at the deepest checkout root above it, and nowhere when there is none —
    /// the workspace root itself is the boot's business.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_event_is_enforced_at_the_deepest_checkout_root_above_it() {
        let roots: HashSet<PathBuf> = ["/ws/a", "/ws/a/b/c"].iter().map(PathBuf::from).collect();
        assert_eq!(
            enclosing_root(&roots, Path::new("/ws/a/b/c/.env")).as_deref(),
            Some(Path::new("/ws/a/b/c"))
        );
        assert_eq!(
            enclosing_root(&roots, Path::new("/ws/a/b/x")).as_deref(),
            Some(Path::new("/ws/a"))
        );
        assert_eq!(
            enclosing_root(&roots, Path::new("/ws/a")).as_deref(),
            Some(Path::new("/ws/a"))
        );
        assert_eq!(
            enclosing_root(&roots, Path::new("/ws/a/b/c/d/e")).as_deref(),
            Some(Path::new("/ws/a/b/c"))
        );
        assert_eq!(enclosing_root(&roots, Path::new("/ws/.env")), None);
    }
}
