// The mock's per-call state slice for the repos feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { ArchMap, RepoMap } from "../../types";
import { DEMO_MAP } from "../repos/mock";
import { OFF_CENTRE_ENTRY_MAP } from "../../cockpit/mapFixtures";
import type { MockState } from "../../mockState";

export type ReposMockState = {
    maps: Map<string, ArchMap>;
    mappings: Map<string, {
        id: string;
        status: import("../../types").SessionStatus;
        created_at: string;
    }>;
    repoMap: (repo: string) => RepoMap;
};

export function installReposMockState(ms: MockState): void {
  // Architecture maps (GET/POST /api/maps): the main repository is already drawn; any other one can
  // be "mapped", which takes a few seconds like a real mapping colony would take minutes.
  // acme/design-system's map has its entry far off the centre (bottom-left), as a real repo's did.
  ms.maps = new Map<string, ArchMap>([
    ["acme/webshop", DEMO_MAP],
    ["acme/design-system", { ...OFF_CENTRE_ENTRY_MAP, title: "acme/design-system" }],
  ]);
  ms.mappings = new Map<string, NonNullable<RepoMap["mapping"]>>();
  ms.repoMap = (repo: string): RepoMap => {
    const map = ms.maps.get(repo);
    return {
      repo,
      map: map ? { repo, revision: "4f2c9e1", generated_at: new Date(Date.now() - 3_600_000).toISOString(), session: "map_demo", map } : null,
      mapping: ms.mappings.get(repo) ?? null,
    };
  };
}
