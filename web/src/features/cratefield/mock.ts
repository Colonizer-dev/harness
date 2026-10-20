// The Cratefield modules catalogue mock (issue #1291): sample modules the mock harness answers with.
// The data structure matches what the real mothership serves when the catalogue is available.
import { clone, sleep } from "../../mockShared";
import type { CratefieldApi } from "./api";
import type { CratefieldCatalog } from "./types";

const CRATEFIELD_CATALOG: CratefieldCatalog = {
  source: "live",
  fetched_at: "2026-10-19T14:32:00Z",
  error: null,
  registry: "live",
  catalogue_url: "https://cratefield.dev/modules",
  modules: [
    {
      slug: "auth",
      name: "Auth",
      summary: "OAuth2 and JWT authentication for your app",
      crate_name: "cratefield-module-auth",
      version: "0.3.1",
      registry_version: "0.3.1",
      status: "published",
      needs: ["Database"],
      optional: ["Cache"],
      depends_on: [],
      tables: ["users", "sessions"],
      links: {
        docs_rs: "https://docs.rs/cratefield-module-auth",
        crate: "https://crates.io/crates/cratefield-module-auth",
        catalogue: "https://cratefield.dev/modules/auth",
      },
    },
    {
      slug: "email",
      name: "Email",
      summary: "Send transactional and marketing emails",
      crate_name: "cratefield-module-email",
      version: "0.1.5",
      registry_version: "0.1.5",
      status: "published",
      needs: ["EmailProvider"],
      optional: ["Database"],
      depends_on: [],
      tables: ["email_log"],
      links: {
        docs_rs: "https://docs.rs/cratefield-module-email",
        crate: "https://crates.io/crates/cratefield-module-email",
        catalogue: "https://cratefield.dev/modules/email",
      },
    },
  ],
};

export function cratefieldMock(): CratefieldApi {
  return {
    cratefieldCatalog: async () => {
      await sleep(150);
      return clone(CRATEFIELD_CATALOG);
    },
  };
}
