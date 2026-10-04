// The mock's per-call state slice for the orgs feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { MemoryNote, OrgSettings } from "../../types";
import type { MockState } from "../../mockState";

export type OrgsMockState = {
    orgSettings: Record<string, OrgSettings>;
    orgAvatars: Record<string, string>;
    awaitingDecision: Set<string>;
    orgOfKey: (note: MemoryNote) => string | null;
};

export function installOrgsMockState(ms: MockState): void {
  ms.orgSettings = {
    acme: {
      agent: { model: "strix/ds4-flash", subagent_model: "deepseek/deepseek-flash", background_model: null },
      max_parallel: 2,
      stack: "rust",
      close_superseded_prs: ["acme/webshop"],
      memory: { enabled: true, deja: true },
      watchdog: { enabled: null, stall_minutes: 10, max_nudges: null },
    },
    // Switched off (issue #176): out of the workspace choices, still reachable via the switcher's Hidden disclosure.
    globex: { enabled: false },
    // Newly appeared and awaiting a decision, so the prompt card shows; it has no colonies yet.
    initech: {},
  };
  // Org avatars, as GET /api/orgs reports them. octocat has none on purpose: an org that only
  // appears in the colony list has no avatar, so its row falls back to the initial.
  ms.orgAvatars = {
    acme: "https://avatars.githubusercontent.com/u/9919?v=4&s=64",
    globex: "https://avatars.githubusercontent.com/u/7654321?v=4&s=64",
    initech: "https://avatars.githubusercontent.com/u/7654322?v=4&s=64",
  };
  // Any explicit save — the prompt card, or a workspace settings save — marks the org decided.
  ms.awaitingDecision = new Set(["initech"]);
  ms.orgOfKey = (note: MemoryNote) => (note.scope === "org" ? note.key : note.scope === "repo" ? note.key.split("/")[0] : null);

}
