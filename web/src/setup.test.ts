// The Setup checklist's derivations: which row is green, which blocks launch, and what each says.
import { describe, expect, it } from "vitest";

import {
  errorDetail,
  firstActionableRow,
  launchEnabled,
  setupProgress,
  setupRows,
  setupTone,
  setupView,
  shouldAutoOpen,
  stackPresetOf,
  type SetupInput,
  type SetupRow,
  type SetupRowId,
} from "./setup";
import type { HarnessStatus, PullStatus, RuntimeInfo, TelemetryStatus } from "./types";

const NODE_IMAGE = "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0";

const DAY = 86_400_000;
/** The clock the Claude row's expiry advisories are pinned to, so the tests never depend on the real one. */
const NOW = Date.parse("2026-09-18T00:00:00Z");

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const LINUX: RuntimeInfo = {
  platform: "linux-x86_64",
  kvm: { ok: true, error: null },
  git: { ok: true, version: "2.45.0" },
  gh: { ok: true, version: "2.60.0" },
  host_claude_bin: "/usr/local/bin/claude",
  host_claude_bin_error: null,
  os: { vendor: "ubuntu", name: "Ubuntu", version: "24.04", id: "ubuntu" },
};

const MAC: RuntimeInfo = {
  platform: "darwin-arm64",
  kvm: null,
  git: { ok: true, version: "2.52.0" },
  gh: { ok: true, version: "2.60.0" },
  host_claude_bin: "/Users/you/.local/bin/claude",
  host_claude_bin_error: null,
  os: { vendor: "apple", name: "macOS", version: "14.5", id: null },
};

/** GET /api/status: a healthy Linux box, with overrides for whatever is broken. */
const status = (over: Partial<HarnessStatus> = {}): HarnessStatus => ({
  github: { connected: true, login: "octocat", name: "The Octocat", source: "gh CLI login" },
  claude: { configured: true, source: "Claude subscription", kind: "CLAUDE_CODE_OAUTH_TOKEN" },
  sandbox: { msb_version: "msb 0.6.18", image: NODE_IMAGE, claude_bin: "/opt/claude/bin/claude", claude_bin_error: null },
  mesh: { enabled: true, provider: "headscale", state: "running", harness_ip: "100.64.0.1", nodes: 2, error: null },
  runtime: { ...LINUX },
  ...over,
});

const pull = (state: PullStatus["state"], error: string | null = null): PullStatus => ({
  image: NODE_IMAGE,
  state,
  started_at: null,
  finished_at: null,
  error,
});

const telemetry = (enabled: boolean | null): TelemetryStatus => ({
  enabled,
  blocked_by: null,
  endpoint: "https://telemetry.colonizer.dev",
  map_url: "https://colonizer.dev/live",
  last_sent_at: null,
  last_error: null,
  heartbeat: { install_id: null, version: "0.1.3", platform: "linux-x86_64", colonies: 0 },
});

/** A healthy status whose Claude object carries the expiry and identity fields the mothership sends
 *  once a token is saved — the shape the row's advisories read. */
const claudeWith = (over: Partial<HarnessStatus["claude"]> = {}): HarnessStatus =>
  status({ claude: { configured: true, source: "Claude subscription", kind: "CLAUDE_CODE_OAUTH_TOKEN", ...over } });

/** A whole Setup input, healthy by default; override any part. */
const input = (over: Partial<SetupInput> = {}): SetupInput => ({
  status: status(),
  pull: pull("cached"),
  telemetry: telemetry(true),
  stackPreset: "auto",
  sessionCount: 0,
  now: NOW,
  ...over,
});

const row = (of: SetupInput, id: SetupRowId): SetupRow => setupRows(of).find((r) => r.id === id)!;

// ---------------------------------------------------------------------------
// setupRows
// ---------------------------------------------------------------------------

describe("setupRows", () => {
  it("lays the rows out in the issue's order, live map last", () => {
    expect(setupRows(input()).map((r) => r.id)).toEqual(["machine", "stack", "github", "claude", "launch", "map"]);
  });

  describe("row 1 — This machine", () => {
    it("collapses to one line naming the versions and paths it found", () => {
      const machine = row(input(), "machine");
      expect(machine.state).toBe("done");
      expect(machine.detail).toContain("Ubuntu 24.04");
      expect(machine.detail).toContain("msb 0.6.18");
      expect(machine.detail).toContain("git 2.45.0");
      expect(machine.detail).toContain("gh 2.60.0");
      expect(machine.detail).toContain("/opt/claude/bin/claude");
      expect(machine.notes).toEqual([]);
    });

    it("stays green on a Mac whose mesh is merely unavailable — the loopback port is by design, not a fault", () => {
      const mac = input({
        status: status({
          runtime: { ...MAC },
          // The pre-#128 payload shape put the explanation in `error` on every Mac.
          mesh: { enabled: true, provider: "headscale", state: "unavailable", harness_ip: null, nodes: 0, error: "tailscaled not vendored" },
        }),
      });
      const machine = row(mac, "machine");
      expect(machine.state).toBe("done");
      expect(machine.gatesLaunch).toBe(true);
      expect(machine.detail).toContain("macOS 14.5");
      expect(machine.notes.join(" ")).toContain("loopback port");
      // Nothing anywhere may read as missing or broken.
      const rows = setupRows(mac);
      expect(rows.every((r) => r.state !== "blocked")).toBe(true);
      expect(rows.every((r) => r.error === undefined)).toBe(true);
      expect(shouldAutoOpen(rows)).toBe(false);
    });

    it("blocks with the mesh error verbatim when the mesh actually failed", () => {
      const bad = input({
        status: status({
          mesh: { enabled: true, provider: "headscale", state: "error", harness_ip: null, nodes: 0, error: "headscale did not start: address already in use" },
        }),
      });
      const machine = row(bad, "machine");
      expect(machine.state).toBe("blocked");
      expect(machine.error).toBe("headscale did not start: address already in use");
      expect(machine.retry).toBe(true);
      expect(launchEnabled(setupRows(bad))).toBe(false);
    });

    it("treats a mesh switched off in Settings as simply not broken", () => {
      const off = input({ status: status({ mesh: { enabled: false, provider: "none", error: null } }) });
      expect(row(off, "machine").state).toBe("done");
    });

    it("blocks on Linux with /dev/kvm unusable, showing the usermod command, and goes green once it reads", () => {
      const broken = input({
        status: status({ runtime: { ...LINUX, kvm: { ok: false, error: "open /dev/kvm: permission denied" } } }),
      });
      const machine = row(broken, "machine");
      expect(machine.state).toBe("blocked");
      expect(machine.command).toBe("sudo usermod -aG kvm $USER");
      expect(machine.fix).toContain("log out");
      expect(machine.error).toContain("permission denied");
      expect(machine.retry).toBe(true);
      expect(shouldAutoOpen(setupRows(broken))).toBe(true);
      expect(launchEnabled(setupRows(broken))).toBe(false);

      // Criterion 5: Check again re-fetches status, and the row turns green with no reload.
      const fixed = input({});
      expect(row(fixed, "machine").state).toBe("done");
      expect(launchEnabled(setupRows(fixed))).toBe(true);
    });

    it("blocks an unsupported platform and offers no Check again", () => {
      const odd = input({ status: status({ runtime: { ...LINUX, platform: "freebsd-amd64" } }) });
      const machine = row(odd, "machine");
      expect(machine.state).toBe("blocked");
      expect(machine.error).toContain("freebsd-amd64");
      expect(machine.retry).toBe(false);
    });

    it("blocks when microsandbox is missing, with the reinstall command", () => {
      const noMsb = input({
        status: status({ sandbox: { msb_version: null, image: NODE_IMAGE, claude_bin: "/opt/claude/bin/claude", claude_bin_error: null } }),
      });
      const machine = row(noMsb, "machine");
      expect(machine.state).toBe("blocked");
      expect(machine.command).toBe("curl -fsSL https://colonizer.dev/install.sh | sh");
      expect(machine.retry).toBe(true);
    });

    it("blocks when git or gh is missing from PATH, with a per-platform install hint", () => {
      const noGh = input({ status: status({ runtime: { ...LINUX, gh: { ok: false, error: "exec: gh: executable file not found in $PATH" } } }) });
      const machine = row(noGh, "machine");
      expect(machine.state).toBe("blocked");
      expect(machine.error).toContain("gh");
      expect(machine.command).toBe("sudo apt install gh");

      const noGitOnMac = input({ status: status({ runtime: { ...MAC, git: { ok: false, error: "git: command not found" } } }) });
      expect(row(noGitOnMac, "machine").command).toBe("brew install git");
    });

    it("blocks when the colony image has no Claude Code binary", () => {
      const noBin = input({
        status: status({ sandbox: { msb_version: "msb 0.6.18", image: NODE_IMAGE, claude_bin: null, claude_bin_error: "claude: not found in image" } }),
      });
      const machine = row(noBin, "machine");
      expect(machine.state).toBe("blocked");
      expect(machine.error).toBe("claude: not found in image");
      expect(machine.retry).toBe(true);
    });

    it("blocks when the mothership cannot write its own files", () => {
      const failing = input({ status: status({ storage: { ok: false, message: "disk quota exceeded", failures: 3 } }) });
      const machine = row(failing, "machine");
      expect(machine.state).toBe("blocked");
      expect(machine.error).toBe("disk quota exceeded");
    });

    it("invents no failures for an older mothership that sends no runtime", () => {
      const payload = status();
      const { runtime: _runtime, ...withoutRuntime } = payload; // the key is simply absent
      const old = input({ status: withoutRuntime });
      const machine = row(old, "machine");
      expect(machine.state).toBe("done");
      expect(machine.error).toBeUndefined();
      expect(row(old, "claude").notes).toEqual([]);
      expect(shouldAutoOpen(setupRows(old))).toBe(false);
      expect(launchEnabled(setupRows(old))).toBe(true);
    });
  });

  describe("row 2 — Stack", () => {
    it("is done once the chosen image is cached, naming the stack", () => {
      const stack = row(input({ stackPreset: "python" }), "stack");
      expect(stack.state).toBe("done");
      expect(stack.detail).toContain("Python");
      expect(stack.gatesLaunch).toBe(false);
    });

    it("shows a running pull as in progress, needing nobody", () => {
      const pulling = input({ pull: pull("pulling") });
      const stack = row(pulling, "stack");
      expect(stack.state).toBe("working");
      expect(stack.detail).toContain(NODE_IMAGE);
      expect(shouldAutoOpen(setupRows(pulling))).toBe(false);
      expect(launchEnabled(setupRows(pulling))).toBe(true);
    });

    it("treats a failed or uncached pull as advisory: launch stays enabled with rows 1, 3 and 4 green", () => {
      for (const state of ["idle", "failed"] as const) {
        const soft = input({ pull: pull(state, state === "failed" ? "no space left on device" : null) });
        const rows = setupRows(soft);
        expect(row(soft, "machine").state).toBe("done");
        expect(row(soft, "github").state).toBe("done");
        expect(row(soft, "claude").state).toBe("done");
        expect(row(soft, "stack").state).not.toBe("blocked");
        expect(row(soft, "stack").gatesLaunch).toBe(false);
        expect(launchEnabled(rows)).toBe(true);
        expect(shouldAutoOpen(rows)).toBe(false);
      }
      // The failed pull still says what happened, and what happens instead.
      const stack = row(input({ pull: pull("failed", "no space left on device") }), "stack");
      expect(stack.error).toBe("no space left on device");
      expect(stack.notes.join(" ")).toContain("when it boots");
    });

    it("defaults to the automatic stack when no preset is saved, and says the repository decides", () => {
      const stack = row(input({ stackPreset: null }), "stack");
      expect(stack.detail).toContain("Automatic");
      expect(stack.notes.join(" ")).toContain("repository");
    });

    it("reads an explicit auto the same way: Automatic, with the per-repository note", () => {
      const stack = row(input({ stackPreset: "auto" }), "stack");
      expect(stack.detail).toContain("Automatic");
      expect(stack.notes.join(" ")).toContain("repository");
    });

    it("adds no per-repository note once a stack is picked by hand", () => {
      expect(row(input({ stackPreset: "node" }), "stack").notes).toEqual([]);
      expect(row(input({ stackPreset: "node" }), "stack").detail).toContain("Node");
    });
  });

  describe("row 3 — GitHub", () => {
    it("names the login it found when connected", () => {
      expect(row(input(), "github")).toMatchObject({ state: "done", detail: "Connected as octocat (gh CLI login)" });
    });

    it("blocks when GitHub is not connected and shows only the first line of a multi-line error", () => {
      const down = input({
        status: status({ github: { connected: false, error: "gh: To get started with GitHub CLI\nrun: gh auth login\ncould not refresh token" } }),
      });
      const github = row(down, "github");
      expect(github.state).toBe("blocked");
      expect(github.gatesLaunch).toBe(true);
      expect(github.error).toBe("gh: To get started with GitHub CLI");
      expect(github.error).not.toContain("could not refresh token");
      expect(github.command).toBe("gh auth login");
      expect(shouldAutoOpen(setupRows(down))).toBe(true);
    });
  });

  describe("row 4 — Claude", () => {
    it("names the login source when configured", () => {
      expect(row(input(), "claude")).toMatchObject({ state: "done", detail: "Claude subscription" });
    });

    it("blocks when Claude is not configured", () => {
      const down = input({ status: status({ claude: { configured: false, source: null, kind: null } }) });
      const claude = row(down, "claude");
      expect(claude.state).toBe("blocked");
      expect(claude.gatesLaunch).toBe(true);
      expect(claude.fix).toContain("setup-token");
      expect(shouldAutoOpen(setupRows(down))).toBe(true);
      expect(launchEnabled(setupRows(down))).toBe(false);
    });

    it("keeps a missing host Claude Code binary advisory, never blocking", () => {
      const macNoBin = input({
        status: status({
          runtime: { ...MAC, host_claude_bin: null, host_claude_bin_error: "exec: claude: executable file not found in $PATH" },
          mesh: { enabled: true, provider: "headscale", state: "unavailable", harness_ip: null, nodes: 0, error: null },
        }),
      });
      const claude = row(macNoBin, "claude");
      expect(claude.state).toBe("done"); // configured — the row is green, the note rides along
      expect(claude.notes.join(" ")).toContain("a saved token still works");
      expect(claude.notes.join(" ")).toContain("executable file not found");
      expect(claude.gatesLaunch).toBe(true);
      expect(shouldAutoOpen(setupRows(macNoBin))).toBe(false);
      expect(launchEnabled(setupRows(macNoBin))).toBe(true);
    });

    it("says nothing about an expiry that is well in the future", () => {
      const future = input({ status: claudeWith({ expires_at: new Date(NOW + 90 * DAY).toISOString() }) });
      const claude = row(future, "claude");
      expect(claude.state).toBe("done");
      expect(claude.notes).toEqual([]);
    });

    it("notes a real expiry within 30 days, without changing anything else about the row", () => {
      const soon = input({ status: claudeWith({ expires_at: new Date(NOW + 12 * DAY).toISOString() }) });
      const claude = row(soon, "claude");
      expect(claude.notes).toEqual(["The Claude token expires in 12 days; sign in again to refresh it."]);
      expect(claude.state).toBe("done");
      expect(claude.gatesLaunch).toBe(true);
      expect(claude.error).toBeUndefined();
    });

    it("draws the 30-day line exactly where the credential panel does — 30 days warns, a hair more does not", () => {
      const at30 = row(input({ status: claudeWith({ expires_at: new Date(NOW + 30 * DAY).toISOString() }) }), "claude");
      expect(at30.notes.join(" ")).toContain("expires in 30 days");
      const justPast30 = row(input({ status: claudeWith({ expires_at: new Date(NOW + 30 * DAY + 1).toISOString() }) }), "claude");
      expect(justPast30.notes).toEqual([]);
    });

    it("counts a partly-lived last day as one day", () => {
      const hoursLeft = row(input({ status: claudeWith({ expires_at: new Date(NOW + 5 * 3_600_000).toISOString() }) }), "claude");
      expect(hoursLeft.notes.join(" ")).toContain("expires in 1 day");
      expect(hoursLeft.notes.join(" ")).not.toContain("days");
    });

    it("words an estimated expiry as a guess, never as fact", () => {
      // The mothership guesses a subscription token's expiry from the save date plus a year;
      // the note has to say so instead of presenting the date as Anthropic's.
      const guessed = input({ status: claudeWith({ expires_at: new Date(NOW + 10 * DAY).toISOString(), expires_estimated: true }) });
      const claude = row(guessed, "claude");
      expect(claude.notes).toEqual([
        "The Claude token is estimated to expire in 10 days — a guess from when the token was saved, not a date Anthropic gave.",
      ]);
      expect(claude.state).toBe("done");
    });

    it("says plainly when the token is already past a real expiry", () => {
      const past = input({ status: claudeWith({ expires_at: new Date(NOW - 3 * DAY).toISOString() }) });
      const claude = row(past, "claude");
      expect(claude.notes).toEqual(["The Claude token expired on 2026-09-15; sign in again to get a fresh one."]);
      expect(claude.state).toBe("done");
    });

    it("does not state a passed estimated expiry as fact either", () => {
      const past = input({ status: claudeWith({ expires_at: new Date(NOW - 3 * DAY).toISOString(), expires_estimated: true }) });
      const claude = row(past, "claude");
      expect(claude.notes).toEqual([
        "The Claude token is past its estimated expiry (2026-09-15) — a guess from when the token was saved, not a date Anthropic gave; sign in again to be sure.",
      ]);
    });

    it("invents no expiry note for a timestamp it cannot parse", () => {
      const unparseable = input({ status: claudeWith({ expires_at: "not a date" }) });
      expect(row(unparseable, "claude").notes).toEqual([]);
    });

    it("passes the mothership's account note through when it could not identify an account", () => {
      const unidentified = input({
        status: claudeWith({
          account: null,
          account_note: "this token is only allowed to make model requests, so Anthropic will not say which account it belongs to",
        }),
      });
      const claude = row(unidentified, "claude");
      expect(claude.notes).toContain(
        "this token is only allowed to make model requests, so Anthropic will not say which account it belongs to",
      );
      expect(claude.detail).toBe("Claude subscription");
      expect(claude.state).toBe("done");
    });

    it("keeps every expiry and account advisory advisory: the row stays green, launch stays on, nothing reads red", () => {
      const advisories: Partial<HarnessStatus["claude"]>[] = [
        { expires_at: new Date(NOW + 12 * DAY).toISOString() },
        { expires_at: new Date(NOW + 12 * DAY).toISOString(), expires_estimated: true },
        { expires_at: new Date(NOW - 3 * DAY).toISOString() },
        { expires_at: new Date(NOW - 3 * DAY).toISOString(), expires_estimated: true },
        { account: null, account_note: "an API key does not identify an account" },
        { expires_at: new Date(NOW - 3 * DAY).toISOString(), account_note: "the token was rejected — it may have expired or been revoked" },
      ];
      for (const advisory of advisories) {
        const withIt = input({ status: claudeWith(advisory) });
        const claude = row(withIt, "claude");
        const which = JSON.stringify(advisory);
        expect(claude.notes.length, which).toBeGreaterThan(0);
        expect(claude.state, which).toBe("done"); // never red
        expect(claude.gatesLaunch, which).toBe(true);
        expect(claude.error, which).toBeUndefined();
        expect(shouldAutoOpen(setupRows(withIt)), which).toBe(false);
        expect(launchEnabled(setupRows(withIt)), which).toBe(true);
        expect(setupView(withIt).launchEnabled, which).toBe(true);
      }
    });
  });

  describe("row 5 — Launch", () => {
    it("is the highlighted exit while no colony exists", () => {
      const launch = row(input({ sessionCount: 0 }), "launch");
      expect(launch.state).toBe("todo");
      expect(launch.title).toBe("Launch");
      expect(launch.gatesLaunch).toBe(false);
      expect(launch.detail).toContain("first colony");
    });

    it("reads Launch a colony and goes quiet once a colony exists", () => {
      const launch = row(input({ sessionCount: 2 }), "launch");
      expect(launch.title).toBe("Launch a colony");
      expect(launch.state).toBe("done");
      expect(launch.detail).toContain("2");
    });
  });

  describe("live map", () => {
    it("asks while the telemetry question is unanswered, and never blocks", () => {
      const unanswered = input({ telemetry: telemetry(null), sessionCount: 1 });
      const map = row(unanswered, "map");
      expect(map.state).toBe("todo");
      expect(map.gatesLaunch).toBe(false);
      expect(shouldAutoOpen(setupRows(unanswered))).toBe(false);
    });

    it("is settled once answered, whichever way it was answered", () => {
      expect(row(input({ telemetry: telemetry(true) }), "map").state).toBe("done");
      expect(row(input({ telemetry: telemetry(false) }), "map").state).toBe("done");
    });
  });
});

// ---------------------------------------------------------------------------
// The helpers the dialog is built from
// ---------------------------------------------------------------------------

describe("setupProgress", () => {
  it("counts the five checklist rows, never the live map", () => {
    // Everything met except the first launch.
    expect(setupProgress(setupRows(input()))).toEqual({ done: 4, total: 5, label: "4 of 5 done" });
    expect(setupProgress(setupRows(input({ sessionCount: 1 })))).toEqual({ done: 5, total: 5, label: "5 of 5 done" });
  });
});

describe("firstActionableRow", () => {
  it("lands on row 4 when rows 1-3 are done and Claude is not yet configured", () => {
    const partial = input({ status: status({ claude: { configured: false, source: null, kind: null } }) });
    expect(firstActionableRow(setupRows(partial))?.id).toBe("claude");
  });

  it("walks past a pull that is running on its own", () => {
    const waiting = input({
      pull: pull("pulling"),
      status: status({ claude: { configured: false, source: null, kind: null } }),
    });
    expect(firstActionableRow(setupRows(waiting))?.id).toBe("claude");
  });

  it("is null when everything is settled", () => {
    expect(firstActionableRow(setupRows(input({ sessionCount: 1 })))).toBeNull();
  });
});

describe("shouldAutoOpen", () => {
  it("opens when a blocking row is unmet", () => {
    expect(shouldAutoOpen(setupRows(input({ status: status({ github: { connected: false } }) })))).toBe(true);
  });

  it("stays closed when only advisories are unmet — an uncached image, an unanswered live map", () => {
    const soft = input({ pull: pull("idle"), telemetry: telemetry(null) });
    expect(shouldAutoOpen(setupRows(soft))).toBe(false);
  });

  it("stays closed when everything is green", () => {
    expect(shouldAutoOpen(setupRows(input({ sessionCount: 3 })))).toBe(false);
  });
});

describe("launchEnabled", () => {
  it("needs rows 1, 3 and 4 green and nothing else", () => {
    expect(launchEnabled(setupRows(input()))).toBe(true);
    expect(launchEnabled(setupRows(input({ status: status({ github: { connected: false } }) })))).toBe(false);
    expect(launchEnabled(setupRows(input({ status: status({ claude: { configured: false, source: null, kind: null } }) })))).toBe(false);
    expect(
      launchEnabled(
        setupRows(input({ status: status({ runtime: { ...LINUX, kvm: { ok: false, error: "/dev/kvm: Permission denied" } } }) })),
      ),
    ).toBe(false);
  });

  it("does not wait for the image pull, however badly it went", () => {
    expect(launchEnabled(setupRows(input({ pull: pull("pulling") })))).toBe(true);
    expect(launchEnabled(setupRows(input({ pull: pull("idle") })))).toBe(true);
    expect(launchEnabled(setupRows(input({ pull: pull("failed", "boom") })))).toBe(true);
  });
});

describe("errorDetail", () => {
  it("keeps the first line up front and the rest behind the disclosure", () => {
    const { head, rest } = errorDetail("gh: To get started with GitHub CLI\nrun: gh auth login\ncould not refresh token");
    expect(head).toBe("gh: To get started with GitHub CLI");
    expect(rest).toBe("run: gh auth login\ncould not refresh token");
  });

  it("has nothing behind the disclosure for a one-line error", () => {
    expect(errorDetail("open /dev/kvm: permission denied")).toEqual({ head: "open /dev/kvm: permission denied", rest: null });
  });

  it("expands from the source payload when the row kept only the first line of a longer one", () => {
    const source = "gh: To get started with GitHub CLI\nrun: gh auth login\ncould not refresh token";
    const { head, rest } = errorDetail("gh: To get started with GitHub CLI", source);
    expect(head).toBe("gh: To get started with GitHub CLI");
    expect(rest).toBe("run: gh auth login\ncould not refresh token");
  });

  it("falls back to the error's own remainder when the source does not carry the head", () => {
    const { rest } = errorDetail("boom\n Details: stack trace", "unrelated payload");
    expect(rest).toBe("Details: stack trace");
  });

  it("says nothing for an empty error", () => {
    expect(errorDetail("\n  \n")).toEqual({ head: "", rest: null });
  });
});

describe("stackPresetOf", () => {
  it("reads a saved preset and reads blank, missing or non-string as null — what null means (automatic) is stackLabel's to say", () => {
    expect(stackPresetOf({ preset: "python", image: "x" })).toBe("python");
    expect(stackPresetOf({ preset: "  " })).toBeNull();
    expect(stackPresetOf({})).toBeNull();
    expect(stackPresetOf({ preset: 4 })).toBeNull();
    expect(stackPresetOf(null)).toBeNull();
    expect(stackPresetOf(undefined)).toBeNull();
  });
});

describe("setupTone", () => {
  it("is red while the checklist can auto-open, green at five of five, amber in between", () => {
    expect(setupTone({ autoOpen: true, progress: { done: 2, total: 5, label: "2 of 5 done" } })).toBe("err");
    expect(setupTone({ autoOpen: false, progress: { done: 5, total: 5, label: "5 of 5 done" } })).toBe("ok");
    expect(setupTone({ autoOpen: false, progress: { done: 4, total: 5, label: "4 of 5 done" } })).toBe("warn");
  });
});

describe("the launch row's image note", () => {
  it("says the colony fetches the image itself whenever it is not already there", () => {
    for (const state of ["idle", "pulling", "failed"] as const) {
      const launch = row(input({ pull: pull(state) }), "launch");
      expect(launch.notes.join(" ")).toContain("waits for the image");
    }
  });

  it("says nothing when the image is already on the machine, or not yet asked", () => {
    expect(row(input({ pull: pull("cached") }), "launch").notes).toEqual([]);
    expect(row(input({ pull: pull("done") }), "launch").notes).toEqual([]);
    expect(row(input({ pull: null }), "launch").notes).toEqual([]);
  });
});

describe("the live map with the telemetry fetch still in the air", () => {
  it("reads as the unanswered question, blocking nothing", () => {
    const unfetched = input({ telemetry: null });
    const map = row(unfetched, "map");
    expect(map.state).toBe("todo");
    expect(map.gatesLaunch).toBe(false);
    expect(shouldAutoOpen(setupRows(unfetched))).toBe(false);
    expect(launchEnabled(setupRows(unfetched))).toBe(true);
  });
});

describe("setupView", () => {
  it("hands over the rows and every summary the shell needs, in one call", () => {
    const view = setupView(input());
    expect(view.rows).toHaveLength(6);
    expect(view.progress).toEqual({ done: 4, total: 5, label: "4 of 5 done" });
    expect(view.firstActionable?.id).toBe("launch");
    expect(view.autoOpen).toBe(false);
    expect(view.launchEnabled).toBe(true);
  });

  it("auto-opens on a runtime failure even with both logins green", () => {
    const view = setupView(input({ status: status({ mesh: { enabled: true, provider: "headscale", state: "error", harness_ip: null, nodes: 0, error: "no mesh" } }) }));
    expect(view.autoOpen).toBe(true);
    expect(view.launchEnabled).toBe(false);
    expect(setupTone(view)).toBe("err");
  });
});
