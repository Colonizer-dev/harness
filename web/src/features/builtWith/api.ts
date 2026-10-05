// The "Built with" list's API methods (issue #944). The root `Api` interface composes `BuiltWithApi`.
import { request } from "../../http";
import type { BuiltWith } from "./types";

export interface BuiltWithApi {
  /**
   * GET /api/built-with (issue #944): the venture's stack — one entry per product, each live or
   * planned, with the registry and venture page the list was taken from so a person can check it.
   */
  builtWith(): Promise<BuiltWith>;
}

export const builtWithHttp: BuiltWithApi = {
  builtWith: () => request("/api/built-with"),
};
