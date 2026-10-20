// The Cratefield modules catalogue API (issue #1291). The root `Api` interface composes `CratefieldApi`.
import { request } from "../../http";
import type { CratefieldCatalog } from "./types";

export interface CratefieldApi {
  /**
   * GET /api/cratefield/catalog (issue #1291): the Cratefield modules catalogue — every module's
   * name, summary, version, status, ports, dependencies and links. The source (live, cache or
   * snapshot) and any fetch error are included, so the UI can warn when the data is stale.
   */
  cratefieldCatalog(): Promise<CratefieldCatalog>;
}

export const cratefieldHttp: CratefieldApi = {
  cratefieldCatalog: () => request("/api/cratefield/catalog"),
};
