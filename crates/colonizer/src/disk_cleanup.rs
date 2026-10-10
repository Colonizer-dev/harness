//! The built-in **Disk cleanup** loop: a maintenance loop present on every install and off by
//! default. Rust and Node builds inside colonies fill disks — ENOSPC has stopped the harness before —
//! so once the owner switches it on, it frees space on a cadence (every hour by default) and early,
//! whenever free space on the data dir's volume drops under a trigger (15% by default).
//!
//! It is a loop with the fixed id [`LOOP_ID`] and `kind: disk_cleanup`, saved beside `loops.json` in
//! its own [`FILE`] (so an older build still reads `loops.json`). It shows in the Loops page and
//! `colonizer loop list`, pauses and resumes with the same switch (`colonizer loop start|stop
//! disk-cleanup`), keeps the loops scheduler's cadence, and runs through
//! `POST /api/loops/disk-cleanup/run-now` — with `?dry_run=1` a preview that lists what would go,
//! with sizes, and removes nothing. Unlike other loops it launches no colony: a run is in-process
//! housekeeping, recorded in the loop's own history and as an activity line.
//!
//! What it may clean, each category with its own switch:
//!
//! - **build output** (on): git-ignored `target/`, `node_modules/`, `.next/` and `dist/` inside the
//!   worktrees of finished colonies — merged, closed, nothing to change, or stopped/failed for N
//!   days — never a colony marked keep-worktree, never a worktree with uncommitted changes, and
//!   for a colony whose work is not on a pull request, never one with unpushed commits;
//! - **worktrees** (on): the automatic reclaim's own candidates and rules (reclaim.rs);
//! - **microVMs** (on): stopped `colonizer-*` microVMs no colony owns, as the reclaim tick removes
//!   them. Images are kept: `msb` has no prune Colonizer can rely on to spare what a colony needs;
//! - **archives** (off): session archive bundles past a keep-days/size limit (archive.rs's plan);
//! - **host paths** (off): Cargo `target/` dirs under directories the owner lists, untouched for
//!   N days, for people who build on the mothership's host.
//!
//! Never: a live, queued, waiting or parked colony, anything with uncommitted or unpushed work,
//! `.git`, `~/.cargo`, package-manager caches, credentials, or anything outside the data dir's
//! worktrees and the listed paths. Symlinks are never followed.
//!
//! Cheap when there is nothing to do: a worktree's build dirs are found once and cached against the
//! colony's `updated_at`, the host paths' walk is cached for hours, and both walks are bounded in
//! depth and in directories visited — so an hourly run over unchanged colonies costs a pass over the
//! in-memory colony list and a `df`.

use crate::loops::{LastRun, Loop, LoopKind};
use crate::schedule::{Cadence, next_run_after};
use crate::sessions::{Session, SessionStatus};
use crate::{App, AppError, Shared, client_error, reclaim, runtime, sandbox, util::dir_size, util::format_disk_size};
use axum::http::StatusCode;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The built-in loop's id, fixed so the CLI and API can name it on every install.
pub const LOOP_ID: &str = "disk-cleanup";
pub const LOOP_NAME: &str = "Disk cleanup";
/// The default cadence: hourly. Any interval the loops scheduler allows (15 minutes up) works.
pub const DEFAULT_INTERVAL_MINUTES: u32 = 60;
/// Run early when free space on the data dir's volume is under this share of the disk.
pub const DEFAULT_TRIGGER_FREE_PCT: u8 = 15;
/// However low the disk, the trigger starts a run no more often than this.
pub const TRIGGER_MIN_GAP_MINUTES: i64 = 15;
/// How often the scheduler's minute tick may probe free space for the trigger.
const TRIGGER_PROBE_MINUTES: i64 = 5;
/// Runs kept in the loop's history, newest first.
pub const HISTORY_KEEP: usize = 30;
/// Paths kept per category in a stored run; the totals always count everything.
const ITEMS_KEPT: usize = 50;
/// Build output directory names inside a worktree. Only git-ignored ones are removed.
pub const BUILD_DIRS: [&str; 4] = ["target", "node_modules", ".next", "dist"];
/// Path components a cleanup never enters: repositories' history, toolchains, package-manager
/// caches and credential stores.
pub const NEVER: [&str; 15] = [
    ".git",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm-store",
    ".yarn",
    ".cache",
    ".ssh",
    ".gnupg",
    ".aws",
    ".docker",
    ".config",
    ".microsandbox",
    ".kube",
    "Library",
];
/// Worktree walks go this deep (a monorepo's `crates/x/target`, `web/apps/y/node_modules`).
const WORKTREE_DEPTH: usize = 5;
/// Host-path walks go this deep under each listed directory.
const HOST_DEPTH: usize = 6;
/// Either walk stops after visiting this many directories: a bound, not a guess at the tree.
const WALK_MAX_DIRS: usize = 20_000;
/// A cached worktree scan is redone after this long even if the colony never changed.
const WORKTREE_RESCAN_HOURS: i64 = 24;
/// A cached host-path scan is redone after this long.
const HOST_RESCAN_HOURS: i64 = 6;
const MAX_EXTRA_PATHS: usize = 20;

/// What a run may clean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    BuildOutput,
    Worktrees,
    Microvms,
    Archives,
    HostPaths,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::BuildOutput,
        Category::Worktrees,
        Category::Microvms,
        Category::Archives,
        Category::HostPaths,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::BuildOutput => "build output",
            Category::Worktrees => "worktrees",
            Category::Microvms => "microVMs",
            Category::Archives => "archives",
            Category::HostPaths => "host paths",
        }
    }
}

/// The owner's choices for the loop. Every field has a default, so a partial body is filled in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Run early when free space is under this percent of the disk; 0 turns the trigger off.
    pub trigger_free_pct: u8,
    pub build_output: bool,
    /// A stopped or failed colony's build output goes only after it has sat this many days.
    pub stopped_after_days: u32,
    pub worktrees: bool,
    pub microvms: bool,
    pub archives: bool,
    /// Archive bundles older than this many days go (with `archives` on).
    pub archive_keep_days: f64,
    /// And oldest-first until the archive holds at most this many gigabytes.
    pub archive_max_gb: Option<f64>,
    /// The host category: owner only, off by default.
    pub host_paths: bool,
    /// Absolute directories whose Cargo `target/` dirs the host category may remove.
    pub extra_paths: Vec<String>,
    /// A host `target/` dir goes only once nothing in its top level changed for this many days.
    pub host_min_age_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            trigger_free_pct: DEFAULT_TRIGGER_FREE_PCT,
            build_output: true,
            stopped_after_days: 7,
            worktrees: true,
            microvms: true,
            archives: false,
            archive_keep_days: 30.0,
            archive_max_gb: None,
            host_paths: false,
            extra_paths: Vec::new(),
            host_min_age_days: 3,
        }
    }
}

impl Settings {
    pub fn enabled(&self, c: Category) -> bool {
        match c {
            Category::BuildOutput => self.build_output,
            Category::Worktrees => self.worktrees,
            Category::Microvms => self.microvms,
            Category::Archives => self.archives,
            Category::HostPaths => self.host_paths,
        }
    }

    /// Refuses settings that would only fail — or reach too far — when the loop runs.
    pub fn check(&self, home: Option<&Path>) -> Result<(), String> {
        if self.trigger_free_pct > 90 {
            return Err(format!(
                "the free-space trigger must be 0 (off) to 90%, got {}%",
                self.trigger_free_pct
            ));
        }
        if !(1..=365).contains(&self.stopped_after_days) {
            return Err("stopped colonies must wait 1 to 365 days before their build output goes".into());
        }
        if !(1..=365).contains(&self.host_min_age_days) {
            return Err("host target/ dirs must be untouched for 1 to 365 days before they go".into());
        }
        if !self.archive_keep_days.is_finite() || self.archive_keep_days < 1.0 {
            return Err("archives must be kept at least 1 day".into());
        }
        if self.archive_max_gb.is_some_and(|g| !g.is_finite() || g <= 0.0) {
            return Err("the archive size limit must be a positive number of gigabytes".into());
        }
        if self.extra_paths.len() > MAX_EXTRA_PATHS {
            return Err(format!("at most {MAX_EXTRA_PATHS} extra paths"));
        }
        for raw in &self.extra_paths {
            check_extra_path(raw, home)?;
        }
        Ok(())
    }
}

/// One listed host directory: absolute, plain (no `.` or `..`), not the filesystem root or the home
/// directory itself, and nowhere near a toolchain, cache or credential store.
fn check_extra_path(raw: &str, home: Option<&Path>) -> Result<(), String> {
    let path = Path::new(raw.trim());
    if !path.is_absolute() {
        return Err(format!("extra path {raw:?} must be absolute"));
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(format!("extra path {raw:?} must not contain . or .."));
    }
    if path.components().count() < 2 || home.is_some_and(|h| h == path) {
        return Err(format!(
            "extra path {raw:?} is too broad: list the directories you build in, not / or your home"
        ));
    }
    if let Some(bad) = forbidden_component(path) {
        return Err(format!(
            "extra path {raw:?} is inside {bad}, which disk cleanup never touches"
        ));
    }
    Ok(())
}

/// The first path component on the [`NEVER`] list, if any.
fn forbidden_component(path: &Path) -> Option<String> {
    path.components().find_map(|c| match c {
        Component::Normal(name) => {
            let name = name.to_string_lossy();
            NEVER.contains(&name.as_ref()).then(|| name.into_owned())
        }
        _ => None,
    })
}

/// One thing a run removed, or would remove.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub path: String,
    /// Unknown for a microVM: `msb` does not say.
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colony: Option<String>,
}

/// Something a run considered and left alone, with why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Held {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CategoryReport {
    pub category: Category,
    pub enabled: bool,
    /// What went (or, in a dry run, would go). Capped at 50 in a stored run; `count` counts all.
    pub items: Vec<Item>,
    pub count: usize,
    /// Bytes freed, or in a dry run the bytes that would be.
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<Held>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CategoryReport {
    fn new(category: Category, enabled: bool) -> Self {
        CategoryReport {
            category,
            enabled,
            items: Vec::new(),
            count: 0,
            bytes: 0,
            held: Vec::new(),
            failed: Vec::new(),
            note: None,
        }
    }

    fn push(&mut self, item: Item) {
        self.count += 1;
        self.bytes += item.bytes.unwrap_or(0);
        self.items.push(item);
    }
}

/// One run of the loop, or one dry run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub at: DateTime<Utc>,
    pub dry_run: bool,
    /// `schedule`, `low_disk` (the free-space trigger) or `manual` (run now).
    pub trigger: String,
    /// Bytes freed across every category, or in a dry run the bytes that would be.
    pub bytes: u64,
    pub categories: Vec<CategoryReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_bytes_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_pct_after: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
}

impl RunReport {
    /// "freed 3.2G (build output 3.1G, worktrees 100M)", for the loop's note and the activity line.
    pub fn summary(&self) -> String {
        let verb = if self.dry_run { "would free" } else { "freed" };
        let parts: Vec<String> = self
            .categories
            .iter()
            .filter(|c| c.count > 0)
            .map(|c| {
                if c.bytes > 0 {
                    format!("{} {}", c.category.label(), format_disk_size(c.bytes))
                } else {
                    format!("{} {}", c.count, c.category.label())
                }
            })
            .collect();
        if parts.is_empty() {
            return if self.dry_run {
                "nothing to clean".into()
            } else {
                "nothing to clean; nothing removed".into()
            };
        }
        format!("{verb} {} ({})", format_disk_size(self.bytes), parts.join(", "))
    }

    fn capped(mut self) -> Self {
        for c in &mut self.categories {
            c.items.truncate(ITEMS_KEPT);
            c.held.truncate(ITEMS_KEPT);
            c.failed.truncate(ITEMS_KEPT);
        }
        self
    }
}

/// The loop's own state, stored on its entry in `loops.json`: its settings (the owner's) and its
/// history and attention (the server's).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub settings: Settings,
    /// Real runs, newest first, at most [`HISTORY_KEEP`]. Dry runs are answered, never stored.
    pub history: Vec<RunReport>,
    /// Set when a run left the disk still under the trigger; cleared by one that did not.
    pub attention: Option<String>,
    /// When a dry run was last answered, so the cockpit knows the owner has seen one.
    pub previewed_at: Option<DateTime<Utc>>,
}

/// The loop's in-memory half on [`App`]: one run at a time, and the scan cache.
#[derive(Default)]
pub struct Runtime {
    running: tokio::sync::Mutex<()>,
    scans: std::sync::Mutex<HashMap<String, Scan>>,
    /// Walks actually performed, for the tests that prove a cached run walks nothing.
    walks: AtomicU64,
    /// Set by the free-space trigger so the run it books says why it ran.
    low_disk_pending: AtomicBool,
    last_probe: std::sync::Mutex<Option<DateTime<Utc>>>,
}

#[derive(Clone)]
struct Scan {
    stamp: String,
    at: DateTime<Utc>,
    found: Vec<(PathBuf, u64)>,
}

impl Runtime {
    /// The cached scan under `key` when it is still good for `stamp`, else `None`.
    fn cached(&self, key: &str, stamp: &str, max_age: ChronoDuration, now: DateTime<Utc>) -> Option<Vec<(PathBuf, u64)>> {
        let scans = self.scans.lock().ok()?;
        let scan = scans.get(key)?;
        (scan.stamp == stamp && now.signed_duration_since(scan.at) < max_age).then(|| scan.found.clone())
    }

    fn store(&self, key: &str, stamp: &str, now: DateTime<Utc>, found: Vec<(PathBuf, u64)>) {
        if let Ok(mut scans) = self.scans.lock() {
            scans.insert(
                key.to_string(),
                Scan {
                    stamp: stamp.to_string(),
                    at: now,
                    found,
                },
            );
        }
    }

    /// Drops removed paths from every cached scan, so the next run does not re-list them.
    fn forget(&self, removed: &[PathBuf]) {
        if let Ok(mut scans) = self.scans.lock() {
            for scan in scans.values_mut() {
                scan.found.retain(|(p, _)| !removed.contains(p));
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn walks(&self) -> u64 {
        self.walks.load(Ordering::Relaxed)
    }
}

/// The built-in loop as a fresh install has it: off, hourly, default settings.
pub fn builtin(now: DateTime<Utc>) -> Loop {
    Loop {
        id: LOOP_ID.into(),
        name: LOOP_NAME.into(),
        org: String::new(),
        repo: String::new(),
        created_by_token: None,
        pending: Vec::new(),
        prompt: String::new(),
        cadence: Cadence::Interval {
            minutes: DEFAULT_INTERVAL_MINUTES,
        },
        kind: LoopKind::DiskCleanup,
        needs_github: false,
        tz_offset_minutes: 0,
        model: None,
        subagent_model: None,
        autopilot: false,
        max_runs: None,
        retry_failed_runs: None,
        end_at: None,
        enabled: false,
        next_run_at: None,
        runs: 0,
        last_run: None,
        last_note: None,
        ended_reason: None,
        created_at: now,
        disk_cleanup: Some(State::default()),
    }
}

/// Where the built-in loop is saved, in the config dir: apart from `loops.json`, so that file never
/// holds a loop kind an older build would refuse to read.
pub const FILE: &str = "disk-cleanup.json";

/// The built-in loop as last saved, or `None` when there is no readable file (a fresh install).
pub fn load_saved(path: &Path) -> Option<Loop> {
    let data = std::fs::read(path).ok()?;
    match serde_json::from_slice::<Loop>(&data) {
        Ok(l) if l.id == LOOP_ID => Some(l),
        Ok(_) => None,
        Err(e) => {
            eprintln!(
                "disk cleanup: could not parse {}: {e}; starting from the defaults",
                path.display()
            );
            None
        }
    }
}

/// Makes sure the built-in loop is in the list — every install has it, off until the owner turns
/// it on — and that an entry saved by an older build carries its state.
pub fn ensure_builtin(loops: &mut Vec<Loop>, now: DateTime<Utc>) {
    match loops.iter_mut().find(|l| l.id == LOOP_ID) {
        Some(l) => {
            l.kind = LoopKind::DiskCleanup;
            l.disk_cleanup.get_or_insert_with(State::default);
        }
        None => loops.push(builtin(now)),
    }
}

// ---------------------------------------------------------------------------
// Free space and the trigger.
// ---------------------------------------------------------------------------

/// The data dir's volume: its size (used + available, as `df`'s capacity counts it) and free bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Disk {
    pub total: u64,
    pub free: u64,
}

impl Disk {
    pub fn used_pct(&self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        (((self.total.saturating_sub(self.free)) as f64 / self.total as f64) * 100.0).round() as u8
    }

    pub fn free_pct(&self) -> f64 {
        if self.total == 0 {
            return 100.0;
        }
        self.free as f64 / self.total as f64 * 100.0
    }

    /// Under the trigger? A trigger of 0 is off.
    pub fn under(&self, trigger_pct: u8) -> bool {
        trigger_pct > 0 && self.free_pct() < f64::from(trigger_pct)
    }
}

pub async fn probe(dir: &Path) -> Option<Disk> {
    let (_, used, avail) = runtime::probe_df(dir).await;
    let (used, avail) = (used?, avail?);
    Some(Disk {
        total: used + avail,
        free: avail,
    })
}

/// Whether the free-space trigger should start a run now: the loop is on, the disk is under its
/// trigger, the loop is not already due, and its last real run is at least
/// [`TRIGGER_MIN_GAP_MINUTES`] old. Unknown free space never triggers. Pure, for the tests.
pub fn trigger_fires(l: &Loop, disk: Option<Disk>, now: DateTime<Utc>) -> bool {
    let Some(state) = l.disk_cleanup.as_ref() else { return false };
    if !l.enabled || !disk.is_some_and(|d| d.under(state.settings.trigger_free_pct)) {
        return false;
    }
    if l.next_run_at.is_none_or(|at| at <= now) {
        return false; // already due, or ended: the scheduler has it
    }
    state
        .history
        .first()
        .is_none_or(|last| now.signed_duration_since(last.at) >= ChronoDuration::minutes(TRIGGER_MIN_GAP_MINUTES))
}

/// The attention item a run leaves when the disk is still under the trigger afterwards. Pure.
pub fn attention_after(trigger_pct: u8, disk: Option<Disk>, live_bytes: u64) -> Option<String> {
    let disk = disk.filter(|d| d.under(trigger_pct))?;
    Some(format!(
        "Disk still {}% full after cleanup — {} in live colonies",
        disk.used_pct(),
        format_disk_size(live_bytes)
    ))
}

/// Called by the loops scheduler every minute, before it looks for due loops: when the built-in
/// loop is on and the disk is under its trigger, books its next run for now. Probes `df` at most
/// every [`TRIGGER_PROBE_MINUTES`], and not at all while the loop is off.
pub(crate) async fn check_trigger(app: &Shared, now: DateTime<Utc>) {
    let Some(l) = app.loops.get(LOOP_ID).await else { return };
    let Some(trigger) = l.disk_cleanup.as_ref().map(|s| s.settings.trigger_free_pct) else {
        return;
    };
    if !l.enabled || trigger == 0 {
        return;
    }
    {
        let Ok(mut last) = app.disk_cleanup.last_probe.lock() else {
            return;
        };
        if last.is_some_and(|at| now.signed_duration_since(at) < ChronoDuration::minutes(TRIGGER_PROBE_MINUTES)) {
            return;
        }
        *last = Some(now);
    }
    let disk = probe(&app.cfg.data_dir).await;
    if trigger_fires(&l, disk, now) {
        let free = disk.map(|d| d.free_pct()).unwrap_or_default();
        app.disk_cleanup.low_disk_pending.store(true, Ordering::Relaxed);
        app.loops
            .update(LOOP_ID, |x| {
                x.next_run_at = Some(now);
                x.last_note = Some(format!(
                    "free space is {free:.0}%, under the {trigger}% trigger: running early"
                ));
            })
            .await;
    }
}

// ---------------------------------------------------------------------------
// Selection: what may go.
// ---------------------------------------------------------------------------

/// What the build-output category makes of one colony.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Not a candidate at all (live, queued, parked, too recent, no worktree): not reported.
    Skip,
    /// A candidate the owner or its state protects, reported as held.
    Held(&'static str),
    /// A candidate. `need_pushed`: its work is not on a pull request, so unpushed commits hold it.
    Eligible { need_pushed: bool },
}

/// The build-output category's rule for one colony. Pure, for the tests.
pub fn build_output_verdict(s: &Session, now: DateTime<Utc>, stopped_after_days: u32) -> Verdict {
    if s.cleaned_up || s.worktree.is_empty() {
        return Verdict::Skip;
    }
    let candidate = match s.status {
        SessionStatus::Merged | SessionStatus::Closed | SessionStatus::NoChanges => true,
        SessionStatus::Stopped | SessionStatus::Failed => {
            now.signed_duration_since(s.updated_at) >= ChronoDuration::days(i64::from(stopped_after_days))
        }
        // Live, queued, waiting, publishing, parked, or a pull request still under review.
        _ => false,
    };
    if !candidate {
        return Verdict::Skip;
    }
    if s.keep_worktree {
        return Verdict::Held("keep-worktree");
    }
    Verdict::Eligible {
        need_pushed: !reclaim::pushed_terminal(s),
    }
}

/// Whether `path` lies strictly inside `root`, both resolved on disk (so a symlink cannot smuggle
/// a path out), and no component under the root is on the [`NEVER`] list.
pub fn within_root(path: &Path, root: &Path) -> bool {
    let (Ok(path), Ok(root)) = (path.canonicalize(), root.canonicalize()) else {
        return false;
    };
    match path.strip_prefix(&root) {
        Ok(rel) => rel.components().next().is_some() && forbidden_component(rel).is_none(),
        Err(_) => false,
    }
}

enum Step {
    Take,
    Descend,
    Skip,
}

/// A breadth-first walk that never follows a symlink, stops at `max_depth` and after
/// [`WALK_MAX_DIRS`] directories, and asks `classify` about every directory it meets.
fn walk(root: &Path, max_depth: usize, mut classify: impl FnMut(&Path, &str) -> Step) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0;
    while let Some((dir, depth)) = queue.pop_front() {
        visited += 1;
        if visited > WALK_MAX_DIRS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            // DirEntry::file_type never follows the entry's own symlink: a link is never a dir here.
            let Ok(kind) = entry.file_type() else { continue };
            if !kind.is_dir() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            match classify(&path, &name) {
                Step::Take => out.push(path),
                Step::Descend if depth + 1 < max_depth => queue.push_back((path, depth + 1)),
                _ => {}
            }
        }
    }
    out
}

/// Build output directories inside a worktree, not yet checked against git.
pub fn find_build_dirs(worktree: &Path) -> Vec<PathBuf> {
    walk(worktree, WORKTREE_DEPTH, |_, name| {
        if NEVER.contains(&name) {
            Step::Skip
        } else if BUILD_DIRS.contains(&name) {
            Step::Take
        } else {
            Step::Descend
        }
    })
}

/// Cargo `target/` dirs under a listed host directory: a `target` beside a `Cargo.toml`, carrying
/// Cargo's own `CACHEDIR.TAG` or `.rustc_info.json`. `skip` holds directories never entered (the
/// data dir, the config dir).
pub fn find_cargo_targets(root: &Path, skip: &[PathBuf]) -> Vec<PathBuf> {
    walk(root, HOST_DEPTH, |path, name| {
        if NEVER.contains(&name) || name == "node_modules" || skip.iter().any(|s| s == path) {
            return Step::Skip;
        }
        if name == "target" {
            let parent_is_crate = path.parent().is_some_and(|p| p.join("Cargo.toml").is_file());
            let cargo_made = path.join("CACHEDIR.TAG").is_file() || path.join(".rustc_info.json").is_file();
            return if parent_is_crate && cargo_made {
                Step::Take
            } else {
                Step::Skip
            };
        }
        Step::Descend
    })
}

/// The newest modification time among a directory and its immediate children: a build touches
/// `target/debug`, `.rustc_info.json` or the fingerprint dirs, so this dates the last build
/// without walking the tree.
fn newest_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = std::fs::symlink_metadata(dir).and_then(|m| m.modified()).ok()?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata().and_then(|m| m.modified())
                && m > newest
            {
                newest = m;
            }
        }
    }
    Some(newest)
}

/// Which of `dirs` git ignores in the worktree. Anything git tracks, or cannot answer for, stays.
async fn git_ignored(worktree: &Path, dirs: &[PathBuf]) -> Vec<bool> {
    let mut out = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let Ok(rel) = dir.strip_prefix(worktree) else {
            out.push(false);
            continue;
        };
        // The clean host default (as in reclaim::work_held): the worktree is colony content.
        let mut cmd = crate::github::git_clean();
        cmd.arg("-C")
            .arg(worktree)
            .args(["check-ignore", "-q", "--"])
            .arg(rel)
            .kill_on_drop(true);
        let ignored = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.status())
            .await
            .is_ok_and(|r| r.is_ok_and(|s| s.success()));
        out.push(ignored);
    }
    out
}

/// A worktree's build dirs with their sizes: from the cache while the colony is unchanged, else
/// walked (and measured) once on the blocking pool.
async fn worktree_build_dirs(app: &Shared, s: &Session, now: DateTime<Utc>) -> Vec<(PathBuf, u64)> {
    let key = format!("worktree:{}", s.id);
    let stamp = s.updated_at.to_rfc3339();
    let fresh = ChronoDuration::hours(WORKTREE_RESCAN_HOURS);
    if let Some(found) = app.disk_cleanup.cached(&key, &stamp, fresh, now) {
        return found.into_iter().filter(|(p, _)| p.is_dir()).collect();
    }
    app.disk_cleanup.walks.fetch_add(1, Ordering::Relaxed);
    let wt = PathBuf::from(&s.worktree);
    let found = tokio::task::spawn_blocking(move || {
        find_build_dirs(&wt)
            .into_iter()
            .map(|p| {
                let bytes = dir_size(&p);
                (p, bytes)
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    app.disk_cleanup.store(&key, &stamp, now, found.clone());
    found
}

/// The build-output plan: every git-ignored build dir in the worktree of a finished colony whose
/// worktree is clean (and, off a pull request, fully pushed).
async fn plan_build_output(app: &Shared, settings: &Settings, sessions: &[Session], now: DateTime<Utc>) -> CategoryReport {
    let mut report = CategoryReport::new(Category::BuildOutput, settings.build_output);
    if !settings.build_output {
        return report;
    }
    let root = app.cfg.data_dir.join("worktrees");
    for s in sessions {
        let need_pushed = match build_output_verdict(s, now, settings.stopped_after_days) {
            Verdict::Skip => continue,
            Verdict::Held(reason) => {
                report.held.push(Held {
                    path: s.worktree.clone(),
                    reason: reason.into(),
                });
                continue;
            }
            Verdict::Eligible { need_pushed } => need_pushed,
        };
        let wt = PathBuf::from(&s.worktree);
        if !within_root(&wt, &root) {
            continue; // not under the data dir's worktrees (or already gone): never touched
        }
        let found = worktree_build_dirs(app, s, now).await;
        if found.is_empty() {
            continue; // the common hourly case: nothing here, no git asked
        }
        if let Some(reason) = reclaim::work_held(&wt, need_pushed).await {
            report.held.push(Held {
                path: s.worktree.clone(),
                reason: reason.into(),
            });
            continue;
        }
        let dirs: Vec<PathBuf> = found.iter().map(|(p, _)| p.clone()).collect();
        let ignored = git_ignored(&wt, &dirs).await;
        for ((path, bytes), ignored) in found.into_iter().zip(ignored) {
            if !ignored || !within_root(&path, &wt) {
                report.held.push(Held {
                    path: path.display().to_string(),
                    reason: "tracked-by-git".into(),
                });
                continue;
            }
            report.push(Item {
                path: path.display().to_string(),
                bytes: Some(bytes),
                colony: Some(s.id.clone()),
            });
        }
    }
    report
}

async fn plan_worktrees(app: &Shared, settings: &Settings, sessions: &[Session], now: DateTime<Utc>) -> CategoryReport {
    let mut report = CategoryReport::new(Category::Worktrees, settings.worktrees);
    if !settings.worktrees {
        return report;
    }
    // The automatic reclaim's own rules and retention: pushed terminal colonies past it, never one
    // marked keep-worktree, never unpushed work.
    let retention = reclaim::ReclaimConfig::load(app).await.retention_secs;
    for s in reclaim::sweep_candidates(sessions, now, retention) {
        let wt = PathBuf::from(&s.worktree);
        let bytes = tokio::task::spawn_blocking(move || dir_size(&wt)).await.unwrap_or(0);
        report.push(Item {
            path: s.worktree.clone(),
            bytes: Some(bytes),
            colony: Some(s.id.clone()),
        });
    }
    report
}

async fn plan_microvms(app: &Shared, settings: &Settings) -> CategoryReport {
    let mut report = CategoryReport::new(Category::Microvms, settings.microvms);
    if !settings.microvms {
        return report;
    }
    match reclaim::orphan_vms(app).await {
        Ok(names) => {
            for name in names {
                report.push(Item {
                    path: name,
                    bytes: None,
                    colony: None,
                });
            }
            report.note =
                Some("microVM images are kept: msb has no prune that can tell which images a colony still needs".into());
        }
        Err(e) => report.note = Some(format!("msb could not list its microVMs, so none were considered: {e:#}")),
    }
    report
}

async fn plan_archives(app: &Shared, settings: &Settings, now: DateTime<Utc>) -> (CategoryReport, Vec<String>) {
    let mut report = CategoryReport::new(Category::Archives, settings.archives);
    if !settings.archives {
        return (report, Vec::new());
    }
    let records = crate::archive::collect_index(&crate::archive::archive_root(app)).await;
    // Switching the category on is the explicit rule archive retention needs to remove the single copy.
    let plan = crate::archive::plan_retention(&records, Some(settings.archive_keep_days), settings.archive_max_gb, true, now);
    let bundles = plan.remove.iter().map(|r| r.bundle.clone()).collect();
    for r in plan.remove {
        report.push(Item {
            path: r.bundle,
            bytes: Some(r.bytes),
            colony: Some(r.session),
        });
    }
    (report, bundles)
}

async fn plan_host_paths(app: &Shared, settings: &Settings, now: DateTime<Utc>) -> CategoryReport {
    let mut report = CategoryReport::new(Category::HostPaths, settings.host_paths);
    if !settings.host_paths {
        return report;
    }
    if settings.extra_paths.is_empty() {
        report.note = Some("no extra paths are listed".into());
        return report;
    }
    let skip: Vec<PathBuf> = [&app.cfg.data_dir, &app.cfg.config_dir]
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    let min_age = std::time::Duration::from_secs(u64::from(settings.host_min_age_days) * 86_400);
    for raw in &settings.extra_paths {
        let Ok(root) = Path::new(raw.trim()).canonicalize() else {
            report.held.push(Held {
                path: raw.clone(),
                reason: "missing".into(),
            });
            continue;
        };
        let key = format!("host:{}", root.display());
        let fresh = ChronoDuration::hours(HOST_RESCAN_HOURS);
        let found = match app.disk_cleanup.cached(&key, "", fresh, now) {
            Some(found) => found.into_iter().filter(|(p, _)| p.is_dir()).collect(),
            None => {
                app.disk_cleanup.walks.fetch_add(1, Ordering::Relaxed);
                let (walk_root, skip) = (root.clone(), skip.clone());
                let found = tokio::task::spawn_blocking(move || {
                    find_cargo_targets(&walk_root, &skip)
                        .into_iter()
                        .map(|p| {
                            let bytes = dir_size(&p);
                            (p, bytes)
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default();
                app.disk_cleanup.store(&key, "", now, found.clone());
                found
            }
        };
        for (path, bytes) in found {
            if !within_root(&path, &root) || skip.iter().any(|s| path.starts_with(s)) {
                continue;
            }
            let age = newest_mtime(&path).and_then(|m| std::time::SystemTime::now().duration_since(m).ok());
            if age.is_none_or(|age| age < min_age) {
                report.held.push(Held {
                    path: path.display().to_string(),
                    reason: "built-recently".into(),
                });
                continue;
            }
            report.push(Item {
                path: path.display().to_string(),
                bytes: Some(bytes),
                colony: None,
            });
        }
    }
    report
}

/// Every category's plan. Removes nothing.
async fn plan(app: &Shared, settings: &Settings, now: DateTime<Utc>) -> (Vec<CategoryReport>, Vec<String>) {
    let sessions = app.sessions.read().await.clone();
    let build = plan_build_output(app, settings, &sessions, now).await;
    let worktrees = plan_worktrees(app, settings, &sessions, now).await;
    let vms = plan_microvms(app, settings).await;
    let (archives, bundles) = plan_archives(app, settings, now).await;
    let host = plan_host_paths(app, settings, now).await;
    (vec![build, worktrees, vms, archives, host], bundles)
}

// ---------------------------------------------------------------------------
// Removal.
// ---------------------------------------------------------------------------

/// Removes one directory the plan named, re-checking at the last moment that it is still a real
/// directory (not a symlink swapped in) inside its root.
async fn remove_dir_checked(path: &Path, root: &Path) -> Result<(), String> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{}: no longer a plain directory", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    if !within_root(path, root) {
        return Err(format!("{}: outside {}", path.display(), root.display()));
    }
    tokio::fs::remove_dir_all(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))
}

async fn execute_build_output(app: &Shared, settings: &Settings, report: &mut CategoryReport, now: DateTime<Utc>) {
    let planned = std::mem::take(&mut report.items);
    (report.count, report.bytes) = (0, 0);
    let mut removed = Vec::new();
    for item in planned {
        let Some(id) = item.colony.clone() else { continue };
        // The colony's lifecycle lock, and its status re-read under it: a resume cannot start on
        // a worktree while its build output is going.
        let lock = app.session_lock(&id).await;
        let _guard = lock.lock().await;
        let still = app.session(&id).await.is_some_and(|s| {
            matches!(
                build_output_verdict(&s, now, settings.stopped_after_days),
                Verdict::Eligible { .. }
            )
        });
        if !still {
            report.held.push(Held {
                path: item.path.clone(),
                reason: "colony-changed".into(),
            });
            continue;
        }
        let Some(wt) = app.session(&id).await.map(|s| PathBuf::from(s.worktree)) else {
            continue;
        };
        let path = PathBuf::from(&item.path);
        match remove_dir_checked(&path, &wt).await {
            Ok(()) => {
                removed.push(path);
                report.push(item);
            }
            Err(e) => report.failed.push(e),
        }
    }
    app.disk_cleanup.forget(&removed);
}

async fn execute_worktrees(app: &Shared, report: &mut CategoryReport) {
    let planned = std::mem::take(&mut report.items);
    (report.count, report.bytes) = (0, 0);
    for item in planned {
        let Some(id) = item.colony.clone() else { continue };
        match reclaim::reclaim_one(app, &id).await {
            Ok(()) => report.push(item),
            Err(e) => report.failed.push(format!("{id}: {e:#}")),
        }
    }
}

async fn execute_microvms(app: &Shared, report: &mut CategoryReport) {
    // Re-listed now: a colony may have claimed one of these names since the plan.
    let orphans = reclaim::orphan_vms(app).await.unwrap_or_default();
    let planned = std::mem::take(&mut report.items);
    report.count = 0;
    for item in planned {
        if orphans.contains(&item.path) {
            sandbox::remove(&app.cfg.msb, &item.path).await;
            report.push(item);
        }
    }
}

async fn execute_archives(app: &Shared, report: &mut CategoryReport) {
    let root = crate::archive::archive_root(app);
    let planned = std::mem::take(&mut report.items);
    (report.count, report.bytes) = (0, 0);
    for item in planned {
        match crate::archive::remove_with_sidecar(&root, &item.path).await {
            Ok(()) => report.push(item),
            Err(e) => report.failed.push(e),
        }
    }
}

async fn execute_host_paths(app: &Shared, settings: &Settings, report: &mut CategoryReport) {
    let roots: Vec<PathBuf> = settings
        .extra_paths
        .iter()
        .filter_map(|p| Path::new(p.trim()).canonicalize().ok())
        .collect();
    let planned = std::mem::take(&mut report.items);
    (report.count, report.bytes) = (0, 0);
    let mut removed = Vec::new();
    for item in planned {
        let path = PathBuf::from(&item.path);
        let Some(root) = roots.iter().find(|r| path.starts_with(r)) else {
            continue;
        };
        match remove_dir_checked(&path, root).await {
            Ok(()) => {
                removed.push(path);
                report.push(item);
            }
            Err(e) => report.failed.push(e),
        }
    }
    app.disk_cleanup.forget(&removed);
}

/// Where a run's free-space reading comes from: `df` on the data dir, or a fixed answer (tests).
pub enum DiskProbe {
    Df,
    #[cfg_attr(not(test), allow(dead_code))]
    Fixed(Option<Disk>),
}

impl DiskProbe {
    async fn read(&self, app: &App) -> Option<Disk> {
        match self {
            DiskProbe::Df => probe(&app.cfg.data_dir).await,
            DiskProbe::Fixed(d) => *d,
        }
    }
}

/// What the live colonies hold on the host, from their last measured footprint.
fn live_bytes(sessions: &[Session]) -> u64 {
    sessions
        .iter()
        .filter(|s| s.status == SessionStatus::Queued || s.status.busy())
        .filter_map(|s| s.host_disk_bytes)
        .sum()
}

/// One run: plan every category, and unless it is a dry run, remove what the plan named and record
/// the run on the loop and in the activity log. One at a time: a second answers 409.
pub(crate) async fn run(app: &Shared, trigger: &str, dry_run: bool, disk: DiskProbe) -> Result<RunReport, AppError> {
    let Ok(_running) = app.disk_cleanup.running.try_lock() else {
        return Err(client_error(
            StatusCode::CONFLICT,
            "disk cleanup is already running; one run at a time",
        ));
    };
    let l = app
        .loops
        .get(LOOP_ID)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such loop"))?;
    let settings = l.disk_cleanup.clone().unwrap_or_default().settings;
    let now = Utc::now();
    let (mut categories, _bundles) = plan(app, &settings, now).await;
    if !dry_run {
        for c in &mut categories {
            match c.category {
                Category::BuildOutput => execute_build_output(app, &settings, c, now).await,
                Category::Worktrees => execute_worktrees(app, c).await,
                Category::Microvms => execute_microvms(app, c).await,
                Category::Archives => execute_archives(app, c).await,
                Category::HostPaths => execute_host_paths(app, &settings, c).await,
            }
        }
    }
    let after = disk.read(app).await;
    let live = live_bytes(&app.sessions.read().await);
    let report = RunReport {
        at: now,
        dry_run,
        trigger: trigger.to_string(),
        bytes: categories.iter().map(|c| c.bytes).sum(),
        categories,
        free_bytes_after: after.map(|d| d.free),
        used_pct_after: after.map(|d| d.used_pct()),
        attention: if dry_run {
            None
        } else {
            attention_after(settings.trigger_free_pct, after, live)
        },
    };
    if dry_run {
        app.loops
            .update(LOOP_ID, |x| {
                if let Some(state) = x.disk_cleanup.as_mut() {
                    state.previewed_at = Some(now);
                }
            })
            .await;
        return Ok(report);
    }
    record(app, &report, now).await;
    Ok(report)
}

/// A real run's record: the loop's count, next slot, note, history and attention, and the
/// activity lines.
async fn record(app: &Shared, report: &RunReport, now: DateTime<Utc>) {
    let summary = report.summary();
    let stored = report.clone().capped();
    let raised = app
        .loops
        .update(LOOP_ID, |x| {
            x.runs += 1;
            x.last_run = Some(LastRun {
                session: String::new(),
                at: now,
                retried: false,
                outcome: None,
            });
            if x.enabled {
                x.next_run_at = Some(next_run_after(&x.cadence, now));
            }
            x.last_note = Some(match &report.attention {
                Some(attention) => format!("{summary} · {attention}"),
                None => summary.clone(),
            });
            let state = x.disk_cleanup.get_or_insert_with(State::default);
            let was = state.attention.is_some();
            state.attention = report.attention.clone();
            state.history.insert(0, stored);
            state.history.truncate(HISTORY_KEEP);
            !was && state.attention.is_some()
        })
        .await
        .is_some_and(|(_, raised)| raised);
    crate::loop_history::record(app, crate::loop_history::from_disk_cleanup(report)).await;
    let mut entry = crate::activity::Entry::new("disk_cleanup.run", "mothership");
    entry.target = Some(LOOP_NAME.into());
    entry.section = Some("loops".into());
    entry.detail = Some(format!("{summary} ({})", report.trigger));
    crate::activity::record(app, entry).await;
    if raised && let Some(attention) = &report.attention {
        let mut entry = crate::activity::Entry::new("disk_cleanup.attention", "mothership");
        entry.target = Some(LOOP_NAME.into());
        entry.section = Some("loops".into());
        entry.detail = Some(attention.clone());
        crate::activity::record(app, entry).await;
        eprintln!("disk cleanup: {attention}");
    }
}

/// The scheduler's firing: a real run, labelled by what started it. A run already under way is a
/// skipped tick that tries again at the next slot.
pub(crate) async fn fire(app: &Shared, now: DateTime<Utc>) {
    let trigger = if app.disk_cleanup.low_disk_pending.swap(false, Ordering::Relaxed) {
        "low_disk"
    } else {
        "schedule"
    };
    if let Err(e) = run(app, trigger, false, DiskProbe::Df).await {
        let message = e.message().to_string();
        app.loops
            .update(LOOP_ID, |x| {
                x.last_note = Some(format!("skipped at {}: {message}", now.format("%H:%M UTC")));
                if x.enabled {
                    x.next_run_at = Some(next_run_after(&x.cadence, now));
                }
            })
            .await;
    }
}

/// The body `PUT /api/loops/disk-cleanup` takes, applied to the built-in loop: its switch, cadence
/// and settings change; its name, id, history and kind do not. A body without `disk_cleanup`
/// (the CLI's `loop start|stop`) keeps the settings it had.
pub(crate) fn apply_update(
    existing: &Loop,
    enabled: Option<bool>,
    cadence: Cadence,
    tz_offset_minutes: Option<i32>,
    settings: Option<Settings>,
    now: DateTime<Utc>,
) -> Result<Loop, AppError> {
    let bad = |m: &str| client_error(StatusCode::BAD_REQUEST, m);
    cadence.check().map_err(|e| bad(&e))?;
    if matches!(cadence, Cadence::SelfPaced {}) {
        return Err(bad(
            "the disk cleanup loop runs on a fixed cadence; self-paced is for colonies",
        ));
    }
    let mut l = existing.clone();
    let mut state = l.disk_cleanup.take().unwrap_or_default();
    if let Some(settings) = settings {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        settings.check(home.as_deref()).map_err(|e| bad(&e))?;
        state.settings = settings;
    }
    let enabled = enabled.unwrap_or(existing.enabled);
    let reschedule = enabled && (!existing.enabled || existing.cadence != cadence || existing.next_run_at.is_none());
    l.cadence = cadence;
    l.enabled = enabled;
    l.tz_offset_minutes = tz_offset_minutes.unwrap_or(existing.tz_offset_minutes);
    if !enabled {
        l.next_run_at = None;
    } else if reschedule {
        l.next_run_at = Some(next_run_after(&l.cadence, now));
    }
    l.ended_reason = None;
    l.disk_cleanup = Some(state);
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;
    use crate::util::short_id;
    use serde_json::json;
    use std::process::Command;

    const GB: u64 = 1024 * 1024 * 1024;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("colonizer-disk-cleanup-{tag}-{}", short_id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A test app whose `msb` does not exist, so no test ever lists or removes a real microVM.
    fn app_at(root: &Path) -> Shared {
        let no_msb = root.join("no-msb").display().to_string();
        std::fs::create_dir_all(root.join("config")).unwrap();
        crate::tests::test_app_with(root, move |cfg| cfg.msb = no_msb)
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} in {}", dir.display());
    }

    /// A committed repository under the app's worktrees root with ignored build output of 4 KiB in
    /// `target/` and 2 KiB in `web/node_modules/`.
    fn worktree(app: &App, slug: &str) -> PathBuf {
        let wt = app.cfg.data_dir.join("worktrees/acme/repo").join(slug);
        std::fs::create_dir_all(wt.join("target/debug")).unwrap();
        std::fs::create_dir_all(wt.join("web/node_modules/pkg")).unwrap();
        std::fs::write(wt.join("target/debug/big"), vec![0u8; 4096]).unwrap();
        std::fs::write(wt.join("web/node_modules/pkg/index.js"), vec![0u8; 2048]).unwrap();
        std::fs::write(wt.join(".gitignore"), "target/\nnode_modules/\n").unwrap();
        std::fs::write(wt.join("README.md"), "hello\n").unwrap();
        git(&wt, &["init", "-q"]);
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "init"]);
        wt
    }

    async fn add_colony(app: &Shared, id: &str, status: SessionStatus, wt: &Path, pr: bool, days_old: i64) {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.worktree = wt.display().to_string();
        s.pr_url = pr.then(|| "https://github.com/acme/repo/pull/1".into());
        s.updated_at = Utc::now() - ChronoDuration::days(days_old);
        app.sessions.write().await.push(s);
    }

    fn only_build_output(settings: &mut Settings) {
        settings.worktrees = false;
        settings.microvms = false;
        settings.archives = false;
        settings.host_paths = false;
    }

    async fn set_settings(app: &Shared, f: impl FnOnce(&mut Settings)) {
        app.loops
            .update(LOOP_ID, |l| {
                f(&mut l.disk_cleanup.get_or_insert_with(State::default).settings)
            })
            .await
            .unwrap();
    }

    fn category(report: &RunReport, c: Category) -> &CategoryReport {
        report.categories.iter().find(|x| x.category == c).unwrap()
    }

    #[test]
    fn the_builtin_loop_is_off_hourly_and_conservative() {
        let l = builtin(Utc::now());
        assert_eq!(l.id, "disk-cleanup");
        assert!(!l.enabled && l.next_run_at.is_none(), "off by default");
        assert_eq!(l.cadence, Cadence::Interval { minutes: 60 });
        let s = l.disk_cleanup.unwrap().settings;
        assert_eq!(s.trigger_free_pct, 15);
        assert!(s.build_output && s.worktrees && s.microvms);
        assert!(!s.archives, "archives hold the only copy: off by default");
        assert!(
            !s.host_paths && s.extra_paths.is_empty(),
            "the host category is off by default"
        );
        let mut loops = vec![];
        ensure_builtin(&mut loops, Utc::now());
        ensure_builtin(&mut loops, Utc::now());
        assert_eq!(loops.len(), 1, "seeded once");
    }

    #[test]
    fn build_output_goes_only_from_finished_colonies() {
        let now = Utc::now();
        let at = |status, pr: bool, days: i64| {
            let mut s = colony("acme", status);
            s.worktree = "/wt".into();
            s.pr_url = pr.then(|| "https://github.com/acme/repo/pull/1".into());
            s.updated_at = now - ChronoDuration::days(days);
            s
        };
        assert_eq!(
            build_output_verdict(&at(SessionStatus::Merged, true, 0), now, 7),
            Verdict::Eligible { need_pushed: false }
        );
        assert_eq!(
            build_output_verdict(&at(SessionStatus::NoChanges, false, 0), now, 7),
            Verdict::Eligible { need_pushed: false }
        );
        assert_eq!(
            build_output_verdict(&at(SessionStatus::Closed, false, 0), now, 7),
            Verdict::Eligible { need_pushed: true },
            "a closed colony with no pull request must prove its commits are pushed"
        );
        for live in [
            SessionStatus::Queued,
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
            SessionStatus::Parked,
            SessionStatus::PrOpened,
        ] {
            assert_eq!(build_output_verdict(&at(live, true, 30), now, 7), Verdict::Skip, "{live:?}");
        }
        // The stopped-for-N-days boundary.
        assert_eq!(
            build_output_verdict(&at(SessionStatus::Stopped, false, 6), now, 7),
            Verdict::Skip
        );
        assert_eq!(
            build_output_verdict(&at(SessionStatus::Stopped, false, 7), now, 7),
            Verdict::Eligible { need_pushed: true }
        );
        assert_eq!(
            build_output_verdict(&at(SessionStatus::Failed, true, 8), now, 7),
            Verdict::Eligible { need_pushed: true },
            "a failed colony may hold commits made after its pull request"
        );
        let mut kept = at(SessionStatus::Merged, true, 30);
        kept.keep_worktree = true;
        assert_eq!(build_output_verdict(&kept, now, 7), Verdict::Held("keep-worktree"));
        let mut gone = at(SessionStatus::Merged, true, 30);
        gone.cleaned_up = true;
        assert_eq!(build_output_verdict(&gone, now, 7), Verdict::Skip);
    }

    #[test]
    fn walks_stay_inside_their_roots_and_never_follow_links() {
        let root = temp_root("walk");
        let outside = temp_root("outside");
        std::fs::create_dir_all(outside.join("target")).unwrap();
        std::fs::create_dir_all(root.join("wt/.git/target")).unwrap();
        std::fs::create_dir_all(root.join("wt/.cargo/registry/node_modules")).unwrap();
        std::fs::create_dir_all(root.join("wt/a/dist")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("wt/escape")).unwrap();
        std::os::unix::fs::symlink(outside.join("target"), root.join("wt/target")).unwrap();
        let found = find_build_dirs(&root.join("wt"));
        assert_eq!(found, vec![root.join("wt/a/dist")], "no .git, no .cargo, no symlinks");
        assert!(!within_root(&outside.join("target"), &root.join("wt")));
        assert!(
            !within_root(&root.join("wt/escape/target"), &root.join("wt")),
            "a symlink out resolves outside"
        );
        assert!(!within_root(&root.join("wt"), &root.join("wt")), "never the root itself");
        assert!(within_root(&root.join("wt/a/dist"), &root.join("wt")));

        // Host targets: only a Cargo-made target beside a Cargo.toml, never inside a skipped dir.
        let host = root.join("code");
        for (krate, tagged) in [("real", true), ("untagged", false)] {
            std::fs::create_dir_all(host.join(krate).join("target/debug")).unwrap();
            std::fs::write(host.join(krate).join("Cargo.toml"), "[package]\n").unwrap();
            if tagged {
                std::fs::write(host.join(krate).join("target/CACHEDIR.TAG"), "Signature").unwrap();
            }
        }
        std::fs::create_dir_all(host.join("data/crate/target")).unwrap();
        std::fs::write(host.join("data/crate/Cargo.toml"), "").unwrap();
        std::fs::write(host.join("data/crate/target/CACHEDIR.TAG"), "").unwrap();
        let targets = find_cargo_targets(&host, &[host.join("data")]);
        assert_eq!(targets, vec![host.join("real/target")]);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn extra_paths_must_be_plain_narrow_and_away_from_caches() {
        let home = Path::new("/home/me");
        let check = |p: &str| check_extra_path(p, Some(home));
        assert!(check("/home/me/code").is_ok());
        for bad in [
            "code",
            "/",
            "/home/me",
            "/home/me/../other",
            "/home/me/.cargo",
            "/home/me/.cache/x",
            "/home/me/.ssh",
        ] {
            assert!(check(bad).is_err(), "{bad} must be refused");
        }
        let mut s = Settings::default();
        assert!(s.check(Some(home)).is_ok());
        s.trigger_free_pct = 95;
        assert!(s.check(Some(home)).is_err());
        s.trigger_free_pct = 0;
        assert!(s.check(Some(home)).is_ok(), "0 turns the trigger off");
    }

    #[test]
    fn the_trigger_fires_under_the_threshold_but_not_twice_in_a_row() {
        let now = Utc::now();
        let mut l = builtin(now);
        let low = Some(Disk {
            total: 100 * GB,
            free: 10 * GB,
        });
        let roomy = Some(Disk {
            total: 100 * GB,
            free: 40 * GB,
        });
        l.next_run_at = Some(now + ChronoDuration::minutes(50));
        assert!(!trigger_fires(&l, low, now), "off by default");
        l.enabled = true;
        assert!(trigger_fires(&l, low, now));
        assert!(!trigger_fires(&l, roomy, now));
        assert!(!trigger_fires(&l, None, now), "no signal is never a full disk");
        let exactly = Some(Disk {
            total: 100 * GB,
            free: 15 * GB,
        });
        assert!(!trigger_fires(&l, exactly, now), "at the threshold is not under it");
        l.disk_cleanup.as_mut().unwrap().settings.trigger_free_pct = 0;
        assert!(!trigger_fires(&l, low, now), "0 turns the trigger off");
        l.disk_cleanup.as_mut().unwrap().settings.trigger_free_pct = 15;
        let recent = RunReport {
            at: now - ChronoDuration::minutes(5),
            dry_run: false,
            trigger: "low_disk".into(),
            bytes: 0,
            categories: vec![],
            free_bytes_after: None,
            used_pct_after: None,
            attention: None,
        };
        l.disk_cleanup.as_mut().unwrap().history.insert(0, recent);
        assert!(!trigger_fires(&l, low, now), "at most every 15 minutes");
        assert!(trigger_fires(&l, low, now + ChronoDuration::minutes(10)));
        l.next_run_at = Some(now);
        assert!(!trigger_fires(&l, low, now), "already due: the scheduler has it");
    }

    #[test]
    fn attention_names_the_fullness_and_what_live_colonies_hold() {
        let low = Some(Disk {
            total: 100 * GB,
            free: 6 * GB,
        });
        assert_eq!(
            attention_after(15, low, 12 * GB).as_deref(),
            Some("Disk still 94% full after cleanup — 12G in live colonies")
        );
        assert_eq!(
            attention_after(
                15,
                Some(Disk {
                    total: 100 * GB,
                    free: 20 * GB
                }),
                0
            ),
            None
        );
        assert_eq!(attention_after(0, low, 0), None, "no trigger, no attention");
        assert_eq!(attention_after(15, None, 0), None);
    }

    #[tokio::test]
    async fn a_dry_run_lists_with_sizes_and_removes_nothing() {
        let root = temp_root("dry");
        let app = app_at(&root);
        let wt = worktree(&app, "merged1");
        add_colony(&app, "merged1", SessionStatus::Merged, &wt, true, 0).await;
        set_settings(&app, only_build_output).await;
        let report = run(&app, "manual", true, DiskProbe::Fixed(None)).await.unwrap();
        let build = category(&report, Category::BuildOutput);
        assert_eq!(build.count, 2, "{build:?}");
        assert!(build.bytes >= 4096 + 2048, "{build:?}");
        assert!(wt.join("target/debug/big").exists() && wt.join("web/node_modules").exists());
        let l = app.loops.get(LOOP_ID).await.unwrap();
        assert_eq!(l.runs, 0, "a dry run is not a run");
        let state = l.disk_cleanup.unwrap();
        assert!(state.history.is_empty() && state.previewed_at.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_run_frees_build_output_records_it_and_skips_what_it_must() {
        let root = temp_root("run");
        let app = app_at(&root);
        // Eligible: merged with a pull request.
        let merged = worktree(&app, "merged1");
        add_colony(&app, "merged1", SessionStatus::Merged, &merged, true, 0).await;
        // Live: never touched.
        let live = worktree(&app, "live1");
        add_colony(&app, "live1", SessionStatus::Running, &live, false, 30).await;
        // Kept by the owner.
        let kept = worktree(&app, "kept1");
        add_colony(&app, "kept1", SessionStatus::Merged, &kept, true, 30).await;
        app.update_session("kept1", |s| s.keep_worktree = true).await;
        // Stopped long ago but with commits no remote has.
        let unpushed = worktree(&app, "stopped1");
        add_colony(&app, "stopped1", SessionStatus::Stopped, &unpushed, false, 30).await;
        // Merged, but with an uncommitted change.
        let dirty = worktree(&app, "dirty1");
        add_colony(&app, "dirty1", SessionStatus::Merged, &dirty, true, 0).await;
        std::fs::write(dirty.join("README.md"), "edited\n").unwrap();
        // A worktree outside the data dir's worktrees root.
        let outside_root = temp_root("elsewhere");
        let elsewhere = outside_root.join("wt");
        std::fs::create_dir_all(elsewhere.join("target")).unwrap();
        add_colony(&app, "elsewhere1", SessionStatus::Merged, &elsewhere, true, 0).await;
        set_settings(&app, only_build_output).await;

        let low = Disk {
            total: 100 * GB,
            free: 5 * GB,
        };
        let report = run(&app, "manual", false, DiskProbe::Fixed(Some(low))).await.unwrap();
        let build = category(&report, Category::BuildOutput);
        assert_eq!(build.count, 2, "{build:?}");
        assert!(!merged.join("target").exists() && !merged.join("web/node_modules").exists());
        assert!(
            merged.join("README.md").exists() && merged.join(".git").exists(),
            "work and .git stay"
        );
        for wt in [&live, &kept, &unpushed, &dirty] {
            assert!(
                wt.join("target/debug/big").exists(),
                "{} must keep its build output",
                wt.display()
            );
        }
        assert!(elsewhere.join("target").exists(), "outside the roots: never");
        let held: Vec<&str> = build.held.iter().map(|h| h.reason.as_str()).collect();
        assert!(held.contains(&"keep-worktree"), "{held:?}");
        assert!(held.contains(&"unpushed-commits"), "{held:?}");
        assert!(held.contains(&"dirty"), "{held:?}");
        assert_eq!(report.bytes, build.bytes);
        assert!(report.bytes >= 4096 + 2048);
        assert_eq!(
            report.attention.as_deref(),
            Some("Disk still 95% full after cleanup — 0B in live colonies")
        );

        let l = app.loops.get(LOOP_ID).await.unwrap();
        assert_eq!(l.runs, 1);
        let state = l.disk_cleanup.unwrap();
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.history[0].trigger, "manual");
        assert!(state.attention.is_some());
        let log = std::fs::read_to_string(app.cfg.data_dir.join(crate::activity::FILE)).unwrap();
        assert!(log.contains("\"disk_cleanup.run\""), "{log}");
        assert!(log.contains("\"disk_cleanup.attention\""), "{log}");

        // Nothing changed since: the next run walks nothing and frees nothing.
        let walks = app.disk_cleanup.walks();
        let again = run(
            &app,
            "schedule",
            false,
            DiskProbe::Fixed(Some(Disk {
                total: 100 * GB,
                free: 50 * GB,
            })),
        )
        .await
        .unwrap();
        assert_eq!(again.bytes, 0);
        assert_eq!(app.disk_cleanup.walks(), walks, "the cached scan is reused");
        let state = app.loops.get(LOOP_ID).await.unwrap().disk_cleanup.unwrap();
        assert!(state.attention.is_none(), "a run above the trigger clears the attention");
        assert_eq!(state.history.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside_root);
    }

    #[tokio::test]
    async fn the_host_category_is_off_by_default_and_spares_recent_builds() {
        let root = temp_root("host");
        let app = app_at(&root);
        let code = root.join("code");
        std::fs::create_dir_all(code.join("app/target/debug")).unwrap();
        std::fs::write(code.join("app/Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(code.join("app/target/CACHEDIR.TAG"), "Signature").unwrap();
        std::fs::write(code.join("app/target/debug/bin"), vec![0u8; 1024]).unwrap();
        let listed = code.display().to_string();
        set_settings(&app, |s| {
            only_build_output(s);
            s.extra_paths = vec![listed.clone()];
        })
        .await;
        let report = run(&app, "manual", false, DiskProbe::Fixed(None)).await.unwrap();
        assert_eq!(category(&report, Category::HostPaths).count, 0, "off by default");
        assert!(code.join("app/target").exists());

        set_settings(&app, |s| s.host_paths = true).await;
        let report = run(&app, "manual", false, DiskProbe::Fixed(None)).await.unwrap();
        let host = category(&report, Category::HostPaths);
        assert_eq!(host.count, 0, "built just now: {host:?}");
        assert_eq!(host.held[0].reason, "built-recently");

        // Age the build, then it goes — and nothing beside it.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86_400);
        for p in ["app/target", "app/target/debug", "app/target/CACHEDIR.TAG"] {
            let f = std::fs::File::open(code.join(p)).unwrap();
            f.set_modified(old).unwrap();
        }
        let report = run(&app, "manual", false, DiskProbe::Fixed(None)).await.unwrap();
        assert_eq!(category(&report, Category::HostPaths).count, 1);
        assert!(!code.join("app/target").exists());
        assert!(code.join("app/Cargo.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn archives_go_only_when_switched_on_and_past_their_age() {
        let root = temp_root("archives");
        let app = app_at(&root);
        let dir = app.cfg.data_dir.join("archive/acme/repo/2026/01");
        std::fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        for (id, days) in [("old", 60), ("new", 1)] {
            let at = now - ChronoDuration::days(days);
            let record = json!({
                "session": id, "repo": "acme/repo", "org": "acme", "issue": null, "title": "", "status": "merged",
                "pr_url": null, "cost_usd": null, "model_usage": null, "model_tier": null, "agent": "",
                "created_at": at, "updated_at": at, "archived_at": at, "mothership": "m", "revision": 1,
                "bundle": format!("acme/repo/2026/01/{id}.tar.zst"), "bytes": 100, "fingerprint": "f"
            });
            std::fs::write(dir.join(format!("{id}.json")), record.to_string()).unwrap();
            std::fs::write(dir.join(format!("{id}.tar.zst")), vec![0u8; 100]).unwrap();
        }
        set_settings(&app, only_build_output).await;
        let report = run(&app, "manual", false, DiskProbe::Fixed(None)).await.unwrap();
        assert_eq!(category(&report, Category::Archives).count, 0);
        set_settings(&app, |s| s.archives = true).await;
        let report = run(&app, "manual", false, DiskProbe::Fixed(None)).await.unwrap();
        assert_eq!(category(&report, Category::Archives).count, 1);
        assert!(!dir.join("old.tar.zst").exists() && !dir.join("old.json").exists());
        assert!(dir.join("new.tar.zst").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn one_run_at_a_time() {
        let root = temp_root("once");
        let app = app_at(&root);
        let _held = app.disk_cleanup.running.lock().await;
        let e = run(&app, "manual", true, DiskProbe::Fixed(None)).await.unwrap_err();
        assert_eq!(e.status(), StatusCode::CONFLICT);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_api_enables_disables_previews_and_refuses_what_it_must() {
        use axum::Json as AxJson;
        use axum::extract::{Path as RoutePath, Query, State as AxState};
        let root = temp_root("api");
        let app = app_at(&root);
        let listed = crate::loops::list(AxState(app.clone()), None).await.0;
        let builtin = listed.iter().find(|l| l.id == LOOP_ID).expect("every install has it");
        assert!(!builtin.enabled);

        let body = |enabled: bool, settings: Option<serde_json::Value>| {
            let mut b = json!({"name": "ignored", "repo": "", "cadence": {"every": "interval", "minutes": 60},
                "kind": "disk_cleanup", "enabled": enabled});
            if let Some(s) = settings {
                b["disk_cleanup"] = s;
            }
            AxJson(serde_json::from_value::<crate::loops::NewLoop>(b).unwrap())
        };
        // Enable, with a partial settings body filled from the defaults.
        let on = crate::loops::update(
            AxState(app.clone()),
            RoutePath(LOOP_ID.into()),
            None,
            body(true, Some(json!({"trigger_free_pct": 20}))),
        )
        .await
        .unwrap()
        .0;
        assert!(on.enabled && on.next_run_at.is_some());
        assert_eq!(on.name, LOOP_NAME, "the name is fixed");
        let settings = &on.disk_cleanup.as_ref().unwrap().settings;
        assert_eq!(settings.trigger_free_pct, 20);
        assert!(settings.build_output && !settings.host_paths);
        // `loop stop` sends no settings: they are kept.
        let off = crate::loops::update(AxState(app.clone()), RoutePath(LOOP_ID.into()), None, body(false, None))
            .await
            .unwrap()
            .0;
        assert!(!off.enabled && off.next_run_at.is_none());
        assert_eq!(off.disk_cleanup.unwrap().settings.trigger_free_pct, 20);
        // Bad settings are refused before anything is saved.
        let bad = crate::loops::update(
            AxState(app.clone()),
            RoutePath(LOOP_ID.into()),
            None,
            body(true, Some(json!({"host_paths": true, "extra_paths": ["/"]}))),
        )
        .await
        .unwrap_err();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert!(!app.loops.get(LOOP_ID).await.unwrap().enabled);

        // The preview: run-now?dry_run=1 answers a report and removes nothing.
        let query = serde_json::from_value::<crate::loops::RunNowQuery>(json!({"dry_run": "1"})).unwrap();
        // An empty body is what the cockpit's preview POST carries (json content-type, no bytes).
        let preview = crate::loops::run_now(
            AxState(app.clone()),
            RoutePath(LOOP_ID.into()),
            Query(query),
            None,
            axum::body::Bytes::new(),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(preview["dry_run"], true);
        assert!(preview["categories"].is_array());
        assert!(preview.get("id").is_none(), "no id: History must not read it as a colony");

        // Built in: never deleted, never made twice.
        let del = crate::loops::delete(AxState(app.clone()), RoutePath(LOOP_ID.into()), None)
            .await
            .unwrap_err();
        assert_eq!(del.status(), StatusCode::CONFLICT);
        let make = json!({"name": "Another", "repo": "acme/web", "cadence": {"every": "interval", "minutes": 60}, "kind": "disk_cleanup"});
        let made = crate::loops::create(AxState(app.clone()), None, AxJson(serde_json::from_value(make).unwrap()))
            .await
            .unwrap_err();
        assert_eq!(made.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_update_switches_the_loop_and_validates_its_settings() {
        let now = Utc::now();
        let l = builtin(now);
        let hourly = Cadence::Interval { minutes: 60 };
        let on = apply_update(&l, Some(true), hourly.clone(), None, None, now).unwrap();
        assert!(on.enabled);
        assert_eq!(on.next_run_at, Some(now + ChronoDuration::minutes(60)));
        assert_eq!(
            on.disk_cleanup.as_ref().unwrap().settings,
            Settings::default(),
            "no body keeps the settings"
        );
        let quarter = apply_update(&on, None, Cadence::Interval { minutes: 15 }, None, None, now).unwrap();
        assert_eq!(
            quarter.next_run_at,
            Some(now + ChronoDuration::minutes(15)),
            "shorter than hourly is allowed"
        );
        assert!(apply_update(&on, None, Cadence::Interval { minutes: 5 }, None, None, now).is_err());
        assert!(apply_update(&on, None, Cadence::SelfPaced {}, None, None, now).is_err());
        let off = apply_update(&on, Some(false), hourly.clone(), None, None, now).unwrap();
        assert!(!off.enabled && off.next_run_at.is_none());
        let bad = Settings {
            extra_paths: vec!["/".into()],
            ..Settings::default()
        };
        assert!(apply_update(&l, None, hourly, None, Some(bad), now).is_err());
    }
}
