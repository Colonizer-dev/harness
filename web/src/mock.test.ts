// The mock models the server's save semantics (issue #176): `PUT /api/orgs/{org}` merges — a field
// the body omits keeps its saved value, one it names (null included) wins — because the prompt card
// answers with `{enabled}` alone and must not clear the org's other settings. If the mock replaced
// wholesale it would hide exactly the bug the merge rule prevents.
import { afterEach, describe, expect, it, vi } from "vitest";

import { createMockApi } from "./mock";
import { SessionStream } from "./sessionStream";

describe("mock spend (issue #209)", () => {
  it("carries the measured org rollup on /api/orgs and keeps an unmeasured one null", async () => {
    const api = createMockApi();
    const acme = (await api.orgs()).find((info) => info.org === "acme");
    expect(acme?.spend?.cost_usd).toBe(44.37);
    expect(acme?.spend?.models[0].model).toBe("claude-opus-5");
    const octo = (await api.orgs()).find((info) => info.org === "octocat");
    expect(octo?.spend?.cost_usd).toBeNull();
  });

  it("serves a deterministic spend history, oldest first, with gap days so the '—' path is exercised", async () => {
    const api = createMockApi();
    const { days } = await api.spendHistory();
    expect(days).toHaveLength(8);
    const [oldest, second] = days;
    expect(oldest.orgs.some((o) => o.org === "acme")).toBe(true);
    expect(days[0].day < days[7].day || days[0].day === days[7].day).toBe(true);
    // A day where only octocat moved: acme's slot for it is a zero-height gap.
    expect(second.orgs.some((o) => o.org === "acme")).toBe(false);
    for (const day of days) {
      for (const o of day.orgs) if (o.org === "octocat") expect(o.cost_usd).toBeNull();
    }
  });

  it("splits each day's model costs/tokens so the stacks agree with the day total", async () => {
    const api = createMockApi();
    const { days } = await api.spendHistory();
    for (const day of days) {
      for (const o of day.orgs) {
        if (o.org !== "acme") continue;
        const dayTokens = o.tokens.input + o.tokens.output + o.tokens.cache_read + o.tokens.cache_write;
        expect(o.models.reduce((n, m) => n + m.tokens, 0)).toBe(dayTokens);
        const priced = o.models.reduce((n, m) => n + (m.cost_usd ?? 0), 0);
        expect(Math.abs(priced - (o.cost_usd ?? 0))).toBeLessThan(0.015);
        // The unpriced routed model keeps cost null instead of a $0.00 slice.
        expect(o.models.find((m) => m.model === "strix/ds4-flash")?.cost_usd).toBeNull();
      }
    }
  });
});

describe("mock saveOrg", () => {
  it("merges like the server: a field the body omits keeps its saved value", async () => {
    const api = createMockApi();
    const before = (await api.orgs()).find((info) => info.org === "acme")?.settings;
    expect(before?.max_parallel).toBe(2);

    // A prompt-card-shaped body: `enabled` only.
    const saved = await api.saveOrg("acme", { enabled: false });
    expect(saved.settings.enabled).toBe(false);
    expect(saved.settings.max_parallel).toBe(2);
    expect(saved.settings.agent?.model).toBe("strix/ds4-flash");
    expect(saved.settings.watchdog?.stall_minutes).toBe(10);

    // The list serves the merged settings, not just the last body.
    const after = (await api.orgs()).find((info) => info.org === "acme");
    expect(after?.settings.enabled).toBe(false);
    expect(after?.settings.max_parallel).toBe(2);
  });

  it("a named null wins, as on the server", async () => {
    const api = createMockApi();
    const saved = await api.saveOrg("acme", { max_parallel: null });
    expect(saved.settings.max_parallel).toBeNull();
  });
});

describe("mock startRedTeamRun", () => {
  it("rejects an armed create on a repo another run is already active on, like the server", async () => {
    const api = createMockApi();
    // The seed has a running raid on acme/webshop, so an armed create there must be refused too —
    // the server's one-active-run-per-repo check does not care how the create is armed.
    await expect(api.startRedTeamRun({ repo: "acme/webshop", arm: true })).rejects.toThrow(
      "a red-team run is already active on this repo",
    );
  });
});

describe("mock synthesizeRedTeamRun (issue #309)", () => {
  it("refuses a run that is not done, is idempotent while pending, and supersedes the old colony on a retry", async () => {
    const api = createMockApi();
    // The seed's rt-demo1 is still raiding acme/webshop.
    await expect(api.synthesizeRedTeamRun("rt-demo1")).rejects.toThrow("synthesis starts once the raid is done");
    // A re-run queues a fresh colony and supersedes the old one; a second call changes nothing.
    const first = await api.synthesizeRedTeamRun("rt-demo2");
    expect(first.synthesis).toMatchObject({ state: "pending", superseded: ["synth7c01", "synth9f2a"] });
    const second = await api.synthesizeRedTeamRun("rt-demo2");
    expect(second.synthesis?.session_id).toBe(first.synthesis?.session_id);
    expect(second.synthesis?.superseded).toEqual(["synth7c01", "synth9f2a"]);
  });
});

describe("mock set_model (issue #240)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("reports the model at start, and a switch only once the colony confirms it", async () => {
    vi.useFakeTimers();
    const api = createMockApi();
    const stream = new SessionStream(api, "demo1234");
    stream.start();
    await vi.advanceTimersByTimeAsync(150);
    expect(stream.getState().model).toBe("claude-opus-5");
    // A second client sees the raw frames, `previous` included.
    const frames: unknown[] = [];
    const raw = api.openEvents("demo1234", 0);
    raw.onmessage = (event) => frames.push(JSON.parse(event.data as string));
    await vi.advanceTimersByTimeAsync(150);

    expect(stream.send({ type: "set_model", model: "sonnet" })).toBe(true);
    expect(stream.getState()).toMatchObject({ model: "claude-opus-5", switchingModel: "sonnet" });
    await vi.advanceTimersByTimeAsync(300);
    expect(stream.getState()).toMatchObject({ model: "sonnet", switchingModel: null });
    expect(frames.filter((f) => (f as { type: string }).type === "model_changed")).toMatchObject([
      { model: "claude-opus-5", previous: null },
      { model: "sonnet", previous: "claude-opus-5" },
    ]);
    raw.close();
    stream.stop();
  });
});

describe("mock stopSession (issue #361)", () => {
  it("answers a second stop with already_stopped, like the server, instead of an error", async () => {
    const api = createMockApi();
    const first = await api.stopSession("demo1234");
    expect(first.result).toBe("stopped");
    expect(first.status).toBe("stopped");
    const second = await api.stopSession("demo1234");
    expect(second.result).toBe("already_stopped");
    expect(second.status).toBe("stopped");
  });

  it("leaves a finished colony's status alone and takes a queued one out of the queue", async () => {
    const api = createMockApi();
    const done = await api.stopSession("old98765");
    expect(done).toMatchObject({ result: "already_stopped", status: "pr_opened" });
    const queued = await api.stopSession("queue1357");
    expect(queued).toMatchObject({ result: "stopped", status: "stopped" });
  });
});
