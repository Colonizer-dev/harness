// The Setup checklist's pure half (issue #129): what each row says, given GET /api/status and the
// few payloads beside it. The dialog renders rows; this module decides what they say, what is
// blocking and what only advisory, and whether Launch may fire. Nothing here touches the browser.
import { meshBroken } from "./components/ui";
import type { HarnessStatus, OsInfo, PullStatus, TelemetryStatus } from "./types";

/** A row's place in the checklist, in display order. The live map is not one of the five. */
export type SetupRowId = "machine" | "stack" | "github" | "claude" | "launch" | "map";

export const SETUP_ROW_IDS: SetupRowId[] = ["machine", "stack", "github", "claude", "launch"];
export const LIVE_MAP_ROW_ID: SetupRowId = "map";

/**
 * Where a row stands.
 *  - `"done"` — met. Collapses to one line naming what was detected (version, path, login).
 *  - `"todo"` — still open and waiting on the person (a stack pick, the first launch, the
 *    live-map question). By itself it never blocks launch.
 *  - `"working"` — progressing on its own right now (the image pull). Needs nobody.
 *  - `"blocked"` — a blocking condition is unmet: red, launch disabled until it is green.
 * Advisory facts never change a row's state; they ride along in `notes`.
 */
export type SetupRowState = "done" | "todo" | "working" | "blocked";

export interface SetupRow {
  id: SetupRowId;
  title: string;
  state: SetupRowState;
  /** Whether an unmet row here stops Launch (rows 1, 3, 4 — never the pull, never the map). */
  gatesLaunch: boolean;
  /** The one line the collapsed row shows, naming what was detected. */
  detail: string;
  /** The OS the machine row sits on, for the small logo next to its title; only the machine row has one. */
  os?: OsInfo;
  /** What to show as the failure. Verbatim, except GitHub, where the issue pins the first line of a multi-line `github.error` (the UI can expand the rest from the source payload). */
  error?: string;
  /** One sentence on what to do about it. */
  fix?: string;
  /** The command to run, where there is one. */
  command?: string;
  /** Non-blocking lines: advisories and platform facts. Shown plainly, never red. */
  notes: string[];
  /** Offer "Check again": a re-fetch of /api/status can change this row's outcome. False for an unsupported platform, where it cannot. */
  retry: boolean;
}

/** Everything the checklist derives from, each straight from its endpoint. */
export interface SetupInput {
  /** GET /api/status. */
  status: HarnessStatus;
  /** GET /api/sandbox/pull; null before the first fetch. */
  pull: PullStatus | null;
  /** GET /api/telemetry; null while the first fetch is still in the air — the same as unanswered. */
  telemetry: TelemetryStatus | null;
  /** `modules.sandbox.settings.preset`; null or blank selects the automatic stack, read off each repository when the colony boots. */
  stackPreset: string | null;
  /** How many sessions exist, any status. */
  sessionCount: number;
  /** The current time, milliseconds since the epoch — threaded in rather than read from the clock
   *  so the Claude row's expiry advisory stays a pure function (the UI passes `Date.now()`). */
  now: number;
}

/** What `setupView` hands the dialog: the rows plus every summary the shell needs. */
export interface SetupView {
  rows: SetupRow[];
  progress: { done: number; total: number; label: string };
  firstActionable: SetupRow | null;
  autoOpen: boolean;
  launchEnabled: boolean;
}

/** The platforms a colony can boot on (docs/install.md); anything else reads as unsupported. */
const SUPPORTED_PLATFORMS = ["linux-x86_64", "darwin-arm64"];

/** The rows, in the order the issue pins: five checklist rows, then the live map. */
export function setupRows(input: SetupInput): SetupRow[] {
  return [
    machineRow(input.status),
    stackRow(input),
    githubRow(input.status),
    claudeRow(input.status, input.now),
    launchRow(input),
    mapRow(input.telemetry),
  ];
}

/** Everything the Setup dialog needs, in one call. */
export function setupView(input: SetupInput): SetupView {
  const rows = setupRows(input);
  return {
    rows,
    progress: setupProgress(rows),
    firstActionable: firstActionableRow(rows),
    autoOpen: shouldAutoOpen(rows, input.sessionCount),
    launchEnabled: launchEnabled(rows),
  };
}

/** The checklist's one-word summary, for the Settings nav dot and the pane header: red while a
 *  blocking row is unmet, green at all five done, amber for a checklist only advisories hold open. */
export function setupTone(view: Pick<SetupView, "autoOpen" | "progress">): "ok" | "warn" | "err" {
  if (view.autoOpen) return "err";
  return view.progress.done === view.progress.total ? "ok" : "warn";
}

/**
 * A verbatim error as a failed row shows it: the first line up front, the rest behind a
 * "more" disclosure. `source` is the payload the row's error came from, for rows that keep
 * only the first line of a longer one (GitHub); its remainder is what the disclosure opens.
 */
export function errorDetail(error: string, source?: string | null): { head: string; rest: string | null } {
  const head = error.split("\n", 1)[0].trim();
  if (!head) return { head, rest: null };
  const at = source?.indexOf(head) ?? -1;
  if (source && at >= 0) {
    const rest = source.slice(at + head.length).replace(/^\r?\n/, "").trim();
    return { head, rest: rest || null };
  }
  const rest = error.split("\n").slice(1).join("\n").trim();
  return { head, rest: rest || null };
}

/**
 * The stack a sandbox module names, as `SetupInput` wants it: null unless `preset` is a
 * non-blank string, and null — no longer the Node stack — is the automatic one, chosen per
 * repository when the colony boots. Settings arrive untyped off the wire.
 */
export function stackPresetOf(settings: Record<string, unknown> | null | undefined): string | null {
  const preset = settings?.preset;
  return typeof preset === "string" && preset.trim() ? preset : null;
}

/** The "n of 5 done" count: the five checklist rows, the live map never counted. */
export function setupProgress(rows: SetupRow[]): { done: number; total: number; label: string } {
  const scored = rows.filter((row) => row.id !== LIVE_MAP_ROW_ID);
  const done = scored.filter((row) => row.state === "done").length;
  return { done, total: scored.length, label: `${done} of ${scored.length} done` };
}

/** The first row that still needs a person — where reopening Setup scrolls to. A row that is
 *  `working` needs nobody, and the map, being last, is only ever the answer when all is done. */
export function firstActionableRow(rows: SetupRow[]): SetupRow | null {
  return rows.find((row) => row.state === "todo" || row.state === "blocked") ?? null;
}

/**
 * Setup auto-opens when a blocking condition is unmet — the runtime failures the old
 * `!github.connected || !claude.configured` check never saw. Advisories do not open it.
 *
 * The one exception is a blocked machine row on a mothership that already has colonies: nothing
 * there was ever about launching into them (KVM is genuinely required to boot a microVM), so
 * once there are colonies to inspect, Setup must not steal the view from them — it was replacing
 * the nest/overview with Settings on every load for platforms Setup calls unsupported and for
 * Linux boxes without a usable /dev/kvm (issue #214). `sessionCount > 0` suppresses exactly that
 * one row: first-run guidance on a truly fresh box (no sessions) still fires, the GitHub and
 * Claude rows keep auto-opening with colonies present, and the blocked row itself plus the
 * disabled Launch stay inside Setup for whoever reaches it.
 */
export function shouldAutoOpen(rows: SetupRow[], sessionCount = 0): boolean {
  return rows.some((row) => row.state === "blocked" && row.gatesLaunch && (row.id !== "machine" || sessionCount === 0));
}

/** Whether Launch may fire: rows 1, 3 and 4 green. Deliberately not gated on the image pull —
 *  the colony fetches the image itself when it boots. */
export function launchEnabled(rows: SetupRow[]): boolean {
  const byId = new Map(rows.map((row) => [row.id, row]));
  return (["machine", "github", "claude"] as const).every((id) => byId.get(id)?.state === "done");
}

// ---------------------------------------------------------------------------
// Row 1 — This machine
// ---------------------------------------------------------------------------

/** What the machine itself can do, probed by the mothership: platform, KVM, msb, git, gh, the
 *  guest Claude Code binary, the mesh and storage. Mesh `unavailable` (a Mac) is a platform
 *  fact, not a fault, and goes in the notes; everything actually broken blocks. */
function machineRow(status: HarnessStatus): SetupRow {
  const runtime = status.runtime;
  const platform = runtime?.platform;
  const kvm = runtime?.kvm ?? null;
  const msb = status.sandbox.msb_version;
  const notes: string[] = [];

  // Blocking findings, most fundamental first; the first one wins the error/fix/command
  // presentation, and later ones are progressively less worth showing (a machine with no
  // KVM has no useful msb verdict beside it).
  const findings: Pick<SetupRow, "error" | "fix" | "command" | "retry">[] = [];
  const block = (finding: (typeof findings)[number]): void => {
    findings.push(finding);
  };

  if (platform !== undefined && !SUPPORTED_PLATFORMS.includes(platform)) {
    block({
      error: `unsupported platform: ${platform}`,
      fix: "Colonizer boots colonies on Linux x86-64 and on Apple-silicon Macs; anything else is not supported.",
      retry: false, // re-checking cannot change the platform
    });
  }

  // KVM is a Linux-only question; on a Mac libkrun needs none, and `kvm` arrives as null.
  if (platform === "linux-x86_64" && kvm && !kvm.ok) {
    block({
      error: kvm.error?.trim() || "the mothership cannot read and write /dev/kvm",
      fix: "Add yourself to the kvm group, then log out and back in for it to take effect.",
      command: "sudo usermod -aG kvm $USER",
      retry: true,
    });
  }

  if (!msb) {
    block({
      fix: "microsandbox (msb) is missing or cannot answer; reinstall Colonizer.",
      command: "curl -fsSL https://colonizer.dev/install.sh | sh",
      retry: true,
    });
  }

  // Git and gh, which colonies use; the install hint follows the platform we are on.
  for (const tool of ["git", "gh"] as const) {
    const probe = tool === "git" ? runtime?.git : runtime?.gh;
    if (probe && !probe.ok) {
      block({
        error: probe.error?.trim() || undefined,
        fix: `${tool} is not on the mothership's PATH; install it so colonies can use it.`,
        command: platform === "darwin-arm64" ? `brew install ${tool}` : platform === "linux-x86_64" ? `sudo apt install ${tool}` : undefined,
        retry: true,
      });
    }
  }

  const guestBinError = status.sandbox.claude_bin_error;
  if (guestBinError) {
    block({
      error: guestBinError,
      fix:
        platform === "darwin-arm64"
          ? "Reinstall Colonizer — the macOS installer bundles the guest Claude Code binary."
          : "Install Claude Code into the colony image.",
      retry: true,
    });
  }

  // meshBroken is the one arbiter of "actually broken": `unavailable` on a Mac is by design (#32),
  // must not read as a fault (#128), and only earns a plain sentence.
  if (meshBroken(status.mesh)) {
    block({
      error: status.mesh?.error?.trim() || "the mesh did not start",
      fix: "Retry the mesh from Settings, or switch it off there; colonies then reach each other over the loopback.",
      retry: true,
    });
  } else if (status.mesh?.enabled && status.mesh.state === "unavailable") {
    notes.push("Colonies are reached on a loopback port on a Mac.");
  }

  if (status.storage && !status.storage.ok) {
    block({
      error: status.storage.message?.trim() || undefined,
      fix: "Free space or fix the disk the storage alert names; the next write that goes through clears this.",
      retry: true, // a later successful write turns `ok` back on (issue #220)
    });
  }

  // The collapsed line names what was found: the OS, versions, then paths.
  const facts = [
    runtime?.os ? [runtime.os.name, runtime.os.version].filter(Boolean).join(" ") : null,
    msb ? (msb.startsWith("msb") ? msb : `msb ${msb}`) : null,
    runtime?.git?.version ? `git ${runtime.git.version}` : null,
    runtime?.gh?.version ? `gh ${runtime.gh.version}` : null,
    platform === "linux-x86_64" && kvm?.ok ? "KVM ready" : null,
    status.sandbox.claude_bin ?? null,
  ].filter((fact): fact is string => fact !== null);

  return {
    id: "machine",
    title: "This machine",
    state: findings.length > 0 ? "blocked" : "done",
    gatesLaunch: true,
    detail: facts.join(" · "),
    os: runtime?.os,
    ...findings[0],
    notes,
    retry: findings[0]?.retry ?? false,
  };
}

// ---------------------------------------------------------------------------
// Row 2 — Stack
// ---------------------------------------------------------------------------

const STACK_LABELS: Record<string, string> = { auto: "Automatic", node: "Node", python: "Python", rust: "Rust", go: "Go" };

const stackLabel = (preset: string | null | undefined): string => {
  const key = preset?.trim() || "auto";
  return STACK_LABELS[key] ?? key.charAt(0).toUpperCase() + key.slice(1);
};

/** The chosen stack and its image. Sits before the logins so the cold pull overlaps them, and
 *  blocks nothing: a missing or failed image is the colony's problem to fetch on boot. */
function stackRow(input: SetupInput): SetupRow {
  const label = stackLabel(input.stackPreset);
  const pull = input.pull;
  let state: SetupRowState;
  let detail: string;
  let error: string | undefined;
  let fix: string | undefined;
  const notes: string[] = [];

  // The row tracks the pre-pull image; the stack it serves is the repository's to decide once a
  // colony boots, so under automatic — unset, blank or an explicit "auto" — say where the
  // decision happens instead of implying Node.
  const preset = input.stackPreset?.trim();
  if (!preset || preset === "auto") {
    notes.push("Each colony's stack is read off its repository when it boots; this image is the Node fallback.");
  }

  switch (pull?.state) {
    case "cached":
    case "done":
      state = "done";
      detail = `${label} image ready · ${pull.image}`;
      break;
    case "pulling":
      state = "working";
      detail = `Downloading the ${label} image · ${pull.image}`;
      break;
    case "failed":
      state = "todo";
      detail = `${label} image is not cached`;
      error = pull.error?.trim() || undefined;
      fix = "Check the image name, then pull it again from Settings.";
      notes.push("The colony will download it when it boots.");
      break;
    case "idle":
      state = "todo";
      detail = `${label} image is not cached`;
      break;
    default:
      // No pull verdict at all: claim nothing about the image either way.
      state = "todo";
      detail = `${label} stack selected`;
      break;
  }

  return { id: "stack", title: "Stack", state, gatesLaunch: false, detail, error, fix, notes, retry: false };
}

// ---------------------------------------------------------------------------
// Rows 3 and 4 — the logins
// ---------------------------------------------------------------------------

/** Warn that a Claude token is nearing its expiry when 30 days or fewer remain — the threshold the
 *  credential panel in Connections.tsx already shows, not a new one invented here. */
const CLAUDE_EXPIRY_WARN_MS = 30 * 86_400_000;

function githubRow(status: HarnessStatus): SetupRow {
  const github = status.github;
  if (github.connected) {
    const who = github.login ?? github.name ?? null;
    return {
      id: "github",
      title: "GitHub",
      state: "done",
      gatesLaunch: true,
      detail: who ? `Connected as ${who}${github.source ? ` (${github.source})` : ""}` : "Connected",
      notes: [],
      retry: false,
    };
  }
  return {
    id: "github",
    title: "GitHub",
    state: "blocked",
    gatesLaunch: true,
    detail: "Not connected",
    error: firstLine(github.error),
    fix: "Run `gh auth login` on this machine, or paste a GitHub token below.",
    command: "gh auth login",
    notes: [],
    retry: true,
  };
}

function claudeRow(status: HarnessStatus, now: number): SetupRow {
  const claude = status.claude;
  const notes: string[] = [];

  // The host binary backs subscription login only; a saved token works without it. Advisory,
  // even when it is the Mac's bundled install that has gone missing.
  const runtime = status.runtime;
  if (runtime) {
    const hostError = runtime.host_claude_bin_error?.trim();
    if (hostError || runtime.host_claude_bin == null) {
      notes.push("Subscription login needs the Claude Code binary on the host; a saved token still works.");
      if (hostError) notes.push(hostError);
    }
  }

  // Expiry (issue #129's advisory table): warned at 30 days or fewer — the threshold the
  // credential panel in Connections.tsx already uses. An estimated expiry — the mothership's
  // save-date-plus-a-year guess — is never stated as fact, and a passed expiry says so plainly.
  // All advisory: a token that is about to expire is not one that has failed, so the row stays
  // green and launch stays enabled.
  const expiresAt = asDate(claude.expires_at);
  if (expiresAt) {
    const msLeft = expiresAt.getTime() - now;
    const estimated = claude.expires_estimated === true;
    const guess = "a guess from when the token was saved, not a date Anthropic gave";
    if (msLeft <= 0) {
      notes.push(
        estimated
          ? `The Claude token is past its estimated expiry (${isoDay(expiresAt)}) — ${guess}; sign in again to be sure.`
          : `The Claude token expired on ${isoDay(expiresAt)}; sign in again to get a fresh one.`,
      );
    } else if (msLeft <= CLAUDE_EXPIRY_WARN_MS) {
      const days = Math.ceil(msLeft / 86_400_000);
      notes.push(
        estimated
          ? `The Claude token is estimated to expire in ${days} ${days === 1 ? "day" : "days"} — ${guess}.`
          : `The Claude token expires in ${days} ${days === 1 ? "day" : "days"}; sign in again to refresh it.`,
      );
    }
  }

  // Why no account could be identified, in the mothership's own words — it explains the identity
  // the row's detail line cannot show, and stays a plain note like every other advisory.
  const accountNote = claude.account_note?.trim();
  if (accountNote) notes.push(accountNote);

  if (claude.configured) {
    return {
      id: "claude",
      title: "Claude",
      state: "done",
      gatesLaunch: true,
      detail: [claude.source ?? "Configured", claude.account ?? null].filter(Boolean).join(" · "),
      notes,
      retry: false,
    };
  }
  return {
    id: "claude",
    title: "Claude",
    state: "blocked",
    gatesLaunch: true,
    detail: "Not configured",
    fix: "Sign in with your Claude subscription, or paste a token from `claude setup-token`.",
    notes,
    retry: true,
  };
}

// ---------------------------------------------------------------------------
// Row 5 and the live map
// ---------------------------------------------------------------------------

function launchRow(input: SetupInput): SetupRow {
  const count = Math.max(0, Math.trunc(input.sessionCount) || 0);
  if (count === 0) {
    const notes: string[] = [];
    // Launch never waits on the pull, so say what happens instead when the image is not there
    // yet: the colony itself fetches it at boot.
    if (input.pull && input.pull.state !== "done" && input.pull.state !== "cached") {
      notes.push("The colony waits for the image and downloads it first if it has to.");
    }
    return {
      id: "launch",
      title: "Launch",
      state: "todo",
      gatesLaunch: false,
      detail: "Launch your first colony — Setup closes and the launch form opens.",
      notes,
      retry: false,
    };
  }
  return {
    id: "launch",
    title: "Launch a colony",
    state: "done",
    gatesLaunch: false,
    detail: `${count} ${count === 1 ? "colony" : "colonies"} so far`,
    notes: [],
    retry: false,
  };
}

/** The live map replaces its standalone prompt: `enabled === null` — or no answer fetched yet —
 *  is the one unanswered question, and either answer settles the row for good. It never gates. */
function mapRow(telemetry: TelemetryStatus | null): SetupRow {
  const notes: string[] = [];
  if (telemetry?.blocked_by) notes.push(`Kept off by the ${telemetry.blocked_by} environment variable.`);

  if (telemetry === null || telemetry.enabled === null) {
    return {
      id: LIVE_MAP_ROW_ID,
      title: "Live map",
      state: "todo",
      gatesLaunch: false,
      detail: "Choose whether Colonizer shares anonymous usage with the live map — nothing personal is sent.",
      fix: "Pick yes or no; you can change it later in Settings.",
      notes,
      retry: false,
    };
  }
  return {
    id: LIVE_MAP_ROW_ID,
    title: "Live map",
    state: "done",
    gatesLaunch: false,
    detail: telemetry.enabled ? "Sharing anonymous usage with the live map." : "Not sharing anonymous usage.",
    notes,
    retry: false,
  };
}

// ---------------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------------

/** The first line of a possibly multi-line error, verbatim apart from its surrounding space; null when there is nothing to show. */
function firstLine(text: string | null | undefined): string | undefined {
  const line = text?.split("\n", 1)[0].trim();
  return line || undefined;
}

/** An ISO timestamp as a Date, or null when it is absent or unparseable — the mothership's own
 *  `asDate` in Connections.tsx reads the field the same guarded way. */
function asDate(ts: string | null | undefined): Date | null {
  if (!ts) return null;
  const at = new Date(ts);
  return Number.isNaN(at.getTime()) ? null : at;
}

/** A date as its UTC calendar day, `YYYY-MM-DD`, so a note's date never leans on the reader's locale. */
function isoDay(at: Date): string {
  return at.toISOString().slice(0, 10);
}
