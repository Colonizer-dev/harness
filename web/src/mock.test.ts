// The mock models the server's save semantics (issue #176): `PUT /api/orgs/{org}` merges — a field
// the body omits keeps its saved value, one it names (null included) wins — because the prompt card
// answers with `{enabled}` alone and must not clear the org's other settings. If the mock replaced
// wholesale it would hide exactly the bug the merge rule prevents.
import { describe, expect, it } from "vitest";

import { createMockApi } from "./mock";

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
