import { describe, expect, it } from "vitest";
import type { ModuleInfo, OrgInfo } from "../types";
import { LOOP_TEMPLATES, describeLoopCadence, effectiveAgentModule, intervalWords, nameFromPrompt, parseInterval, parseLoopCommand, relative, selfPacedWarning, toLocalChoice, toUtcLoopCadence } from "./loops";

describe("loops", () => {
  it("parses intervals the way people type them", () => {
    expect(parseInterval("15m")).toBe(15);
    expect(parseInterval("2h")).toBe(120);
    expect(parseInterval("1.5h")).toBe(90);
    expect(parseInterval("1d")).toBe(1440);
    expect(parseInterval("90 minutes")).toBe(90);
    expect(parseInterval("check")).toBeNull();
  });

  it("reads /loop like Claude Code: an interval, or self-paced without one", () => {
    expect(parseLoopCommand("/loop 1h check CI on main and fix flakes")).toEqual({
      cadence: { every: "interval", minutes: 60 },
      prompt: "check CI on main and fix flakes",
      error: null,
    });
    expect(parseLoopCommand("/loop keep the docs in step with the code")).toEqual({
      cadence: { every: "self_paced" },
      prompt: "keep the docs in step with the code",
      error: null,
    });
    expect(parseLoopCommand("/loop 5m poll")?.error).toMatch(/15 minutes/);
    expect(parseLoopCommand("/loop")?.error).toMatch(/Say what the loop should do/);
    expect(parseLoopCommand("fix the build")).toBeNull();
  });

  it("reads whole-day intervals past a week as every-N-days, anchored at this UTC time", () => {
    const now = new Date(Date.UTC(2026, 8, 24, 3, 5));
    expect(parseLoopCommand("/loop 14d check the map", now)).toEqual({
      cadence: { every: "every_days", days: 14, hour: 3, minute: 5 },
      prompt: "check the map",
      error: null,
    });
    for (const days of [30, 60]) {
      expect(parseLoopCommand(`/loop ${days}d sweep the backlog`, now)?.cadence).toEqual({ every: "every_days", days, hour: 3, minute: 5 });
    }
    // Up to and including a week it is still the minute interval it always was.
    expect(parseLoopCommand("/loop 7d sweep the backlog", now)?.cadence).toEqual({ every: "interval", minutes: 10080 });
    expect(parseLoopCommand("/loop 366d sweep the backlog", now)?.error).toMatch(/365/);
  });

  it("describes cadences in words", () => {
    expect(describeLoopCadence({ every: "interval", minutes: 15 })).toBe("every 15 minutes");
    expect(describeLoopCadence({ every: "interval", minutes: 120 })).toBe("every 2 hours");
    expect(describeLoopCadence({ every: "self_paced" })).toMatch(/self-paced/);
    expect(intervalWords(1440)).toBe("day");
  });

  it("round-trips a daily local time through UTC", () => {
    const now = new Date(2026, 8, 24, 12, 0);
    const utc = toUtcLoopCadence({ every: "daily", time: "09:30" }, now);
    expect(utc.every).toBe("daily");
    expect(toLocalChoice(utc, now)).toEqual({ every: "daily", time: "09:30" });
    expect(toUtcLoopCadence({ every: "interval", minutes: 3 })).toEqual({ every: "interval", minutes: 15 });
  });

  it("round-trips an every-N-days local time through UTC, crossing midnight when the zone pushes it", () => {
    const now = new Date(2026, 8, 24, 0, 30);
    const utc = toUtcLoopCadence({ every: "every_days", days: 14, time: "23:30" }, now);
    // The stored UTC time is the local one minus the zone's offset from UTC, wrapping past midnight as
    // needed (23:30 in UTC+8 is 15:30 UTC; in UTC-5 it is 04:30 the next day).
    const shift = -now.getTimezoneOffset();
    const minutes = (23 * 60 + 30 - shift + 1440) % 1440;
    expect(utc).toEqual({ every: "every_days", days: 14, hour: Math.floor(minutes / 60), minute: minutes % 60 });
    expect(toLocalChoice(utc, now)).toEqual({ every: "every_days", days: 14, time: "23:30" });
    expect(describeLoopCadence(utc, now)).toBe("every 14 days at 23:30");
    expect(describeLoopCadence({ every: "every_days", days: 1, hour: 0, minute: 0 }, now)).toMatch(/every 1 day at /);
  });

  it("names a loop from its prompt and says how far off a run is", () => {
    expect(nameFromPrompt("Triage new issues\nand more")).toBe("Triage new issues");
    expect(nameFromPrompt("x".repeat(80)).endsWith("…")).toBe(true);
    // The cut falls on the emoji's surrogate pair; a lone half would fail the server's JSON parse.
    const emoji = nameFromPrompt("Every morning, check the nightly build and post summary 🚀 to the team channel");
    expect(emoji).toBe("Every morning, check the nightly build and post summary 🚀…");
    expect(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/.test(emoji)).toBe(false);
    const now = Date.UTC(2026, 8, 24, 12, 0);
    expect(relative(new Date(now + 3 * 3600_000).toISOString(), now)).toBe("in 3h");
    expect(relative(new Date(now - 20 * 60_000).toISOString(), now)).toBe("20m ago");
    expect(relative(null, now)).toBe("—");
  });

  it("warns about a self-paced loop only when the org's agent module serves neither loop tool", () => {
    // What GET /api/modules says: agent rows carry loop_tools (issue #643), other kinds do not.
    const agent = (loopTools?: boolean, provider = "claude-code"): ModuleInfo => ({
      kind: "agent",
      provider,
      enabled: true,
      settings: {},
      schema: null,
      providers: [
        { id: "claude-code", name: "Claude Code", loop_tools: true },
        { id: "pi", name: "Pi", ...(loopTools === undefined ? {} : { loop_tools: loopTools }) },
      ],
    });
    const orgWith = (org: string, module: string | null): OrgInfo =>
      ({ org, settings: { agent: { module } } }) as OrgInfo;
    const orgs = [orgWith("acme", null), orgWith("globex", "pi")];
    const modules = [agent()];
    // The mothership's own pick (claude-code) serves the tools: no warning.
    expect(selfPacedWarning("acme", orgs, modules)).toBeNull();
    // An org pick without them warns, naming the module; the org match is case-insensitive.
    expect(selfPacedWarning("GLOBEX", orgs, modules)).toMatch(/agent module \(Pi\)/);
    // The resolution mirrors the server: the org's pick, else the mothership's provider.
    expect(effectiveAgentModule("globex", orgs, modules)?.id).toBe("pi");
    expect(effectiveAgentModule("acme", orgs, modules)?.id).toBe("claude-code");
    expect(effectiveAgentModule("acme", [orgWith("acme", " ")], modules)?.id).toBe("claude-code");
    // A row without the flag reads as lacking the tools; no agent kind at all says nothing.
    expect(selfPacedWarning("globex", orgs, [agent(false)])).toMatch(/can't pace its own loop/);
    expect(selfPacedWarning("globex", orgs, [])).toBeNull();
  });

  it("offers a data-refresh template that needs GitHub and names its inputs", () => {
    const t = LOOP_TEMPLATES.find((x) => x.label === "Refresh data files from their sources");
    expect(t).toBeDefined();
    expect(t?.needsGithub).toBe(true);
    expect(t?.choice).toEqual({ every: "daily", time: "05:00" });
    // Every input the colony is told to read and run, so editing the prompt cannot drop one.
    for (const input of ["data/sources.json", "npm run extract -- <id>", "npm run validate", "npm run --silent refresh-policy", "evidence/", "/harness/out/pr-labels", "source-broken:"]) {
      expect(t?.prompt).toContain(input);
    }
  });
});
