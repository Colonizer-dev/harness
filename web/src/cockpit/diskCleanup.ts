// The built-in Disk cleanup loop (disk_cleanup.rs): present on every install, off until the owner
// switches it on. Pure helpers here, so diskCleanup.test.ts owns the rules and the row and dialog in
// DiskCleanupLoop.tsx stay renderers.
import type { DiskCleanupCategory, DiskCleanupReport, DiskCleanupSettings, Loop, NewLoop } from "../types";
import { formatBytes } from "./host";
import { intervalWords } from "./loops";

/** The built-in loop's fixed id: `colonizer loop enable disk-cleanup` names it too. */
export const DISK_CLEANUP_ID = "disk-cleanup";

/** The default cadence, in minutes: hourly. The loops scheduler allows 15 minutes and up. */
export const DISK_CLEANUP_DEFAULT_MINUTES = 60;
export const DISK_CLEANUP_MIN_MINUTES = 15;

export const DEFAULT_DISK_CLEANUP_SETTINGS: DiskCleanupSettings = {
  trigger_free_pct: 15,
  build_output: true,
  stopped_after_days: 7,
  worktrees: true,
  microvms: true,
  archives: false,
  archive_keep_days: 30,
  archive_max_gb: null,
  host_paths: false,
  extra_paths: [],
  host_min_age_days: 3,
};

/** Each category's switch label and what it removes, in the order the dialog lists them. */
export const DISK_CLEANUP_CATEGORIES: { key: DiskCleanupCategory; label: string; hint: string }[] = [
  { key: "build_output", label: "Build output", hint: "git-ignored target/, node_modules/, .next/ and dist/ in finished colonies' worktrees" },
  { key: "worktrees", label: "Worktrees", hint: "finished colonies' worktrees past the reclaim retention, never unpushed work or keep-worktree" },
  { key: "microvms", label: "MicroVMs", hint: "stopped microVMs no colony owns (images are kept)" },
  { key: "archives", label: "Session archives", hint: "archived session bundles older than the keep-days limit — the only copy" },
  { key: "host_paths", label: "Host build dirs", hint: "Cargo target/ dirs under the paths you list, untouched for a few days" },
];

export function isDiskCleanup(l: Pick<Loop, "id" | "kind">): boolean {
  return l.kind === "disk_cleanup" || l.id === DISK_CLEANUP_ID;
}

export function settingsOf(l: Pick<Loop, "disk_cleanup">): DiskCleanupSettings {
  return { ...DEFAULT_DISK_CLEANUP_SETTINGS, ...(l.disk_cleanup?.settings ?? {}) };
}

/**
 * What the switch does when pressed. Turning the loop on before the owner has ever seen a dry run
 * shows the preview first — what the first run would remove, with sizes — and the loop is enabled
 * only from there. Turning it off, or on again after a preview, just saves.
 */
export function toggleAction(l: Pick<Loop, "disk_cleanup">, on: boolean): "preview" | "save" {
  return on && !l.disk_cleanup?.previewed_at ? "preview" : "save";
}

/** The PUT body for the built-in loop: its cadence, switch and settings. */
export function diskCleanupBody(l: Loop, change: { enabled?: boolean; minutes?: number; settings?: DiskCleanupSettings } = {}): NewLoop {
  const minutes = change.minutes ?? (l.cadence.every === "interval" ? l.cadence.minutes : DISK_CLEANUP_DEFAULT_MINUTES);
  return {
    name: l.name,
    repo: l.repo,
    prompt: "",
    cadence: { every: "interval", minutes },
    kind: "disk_cleanup",
    tz_offset_minutes: -new Date().getTimezoneOffset(),
    enabled: change.enabled ?? l.enabled,
    disk_cleanup: change.settings ?? settingsOf(l),
  };
}

/** The loop's cadence and early trigger, as the card's schedule line says it. */
export function describeDiskCleanup(l: Loop): string {
  const s = settingsOf(l);
  const minutes = l.cadence.every === "interval" ? l.cadence.minutes : DISK_CLEANUP_DEFAULT_MINUTES;
  const trigger = s.trigger_free_pct > 0 ? ` · early under ${s.trigger_free_pct}% free` : "";
  return `every ${intervalWords(minutes)}${trigger}`;
}

/** "would free 3.2G", "freed 3.2G", or that there was nothing to clean. */
export function reportSummary(r: Pick<DiskCleanupReport, "dry_run" | "bytes" | "categories">): string {
  const items = r.categories.reduce((n, c) => n + c.count, 0);
  if (items === 0) return "nothing to clean";
  return `${r.dry_run ? "would free" : "freed"} ${formatBytes(r.bytes)} · ${items} ${items === 1 ? "item" : "items"}`;
}

export function categoryLabel(key: DiskCleanupCategory): string {
  return DISK_CLEANUP_CATEGORIES.find((c) => c.key === key)?.label ?? key;
}

/** Why something was kept, in words. */
export function heldReason(reason: string): string {
  switch (reason) {
    case "keep-worktree":
      return "marked keep worktree";
    case "dirty":
      return "uncommitted changes";
    case "unpushed-commits":
      return "unpushed commits";
    case "unreadable-git":
      return "git could not say";
    case "tracked-by-git":
      return "tracked by git";
    case "built-recently":
      return "built recently";
    case "colony-changed":
      return "the colony changed";
    case "missing":
      return "path missing";
    default:
      return reason;
  }
}

/** The extra-paths textarea: one absolute path per line, blanks dropped. */
export function parseExtraPaths(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
}
