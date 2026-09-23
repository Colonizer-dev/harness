// The mock models the server's save semantics (issue #176): `PUT /api/orgs/{org}` merges — a field
// the body omits keeps its saved value, one it names (null included) wins — because the prompt card
// answers with `{enabled}` alone and must not clear the org's other settings. If the mock replaced
// wholesale it would hide exactly the bug the merge rule prevents.
import { describe, expect, it } from "vitest";

import { createMockApi } from "./mock";

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
