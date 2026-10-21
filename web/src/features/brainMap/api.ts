// The brain map's API client (crates/colonizer/src/living_brain/map.rs). The root `Api` interface composes it.
import { query, request } from "../../http";
import type { BrainMapAnswer } from "./types";

export interface BrainMapRequest {
  /** Replace every label with its hash (the server does this for every remote request anyway). */
  structure?: boolean;
  /** `owner/name` repositories to find on the map. */
  repos?: string[];
}

export interface BrainMapApi {
  /** GET /api/brain/graph: the brain's structure for the 3D view; never the token, never page content. */
  brainMap(body?: BrainMapRequest): Promise<BrainMapAnswer>;
}

export const brainMapHttp: BrainMapApi = {
  brainMap: ({ structure = false, repos = [] } = {}) =>
    request(`/api/brain/graph${query({ structure: structure ? "1" : "", repos: repos.join(",") })}`),
};
