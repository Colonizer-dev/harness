// Cratefield modules pane tests (issue #1291): filtering, status chips, optional ports, and
// catalogue notice (snapshot with date + error, cache, invalid date, live → null).
import { describe, expect, it } from "vitest";

import type { CratefieldModule } from "../../types";
import { catalogueNotice, filterModules, portsOf } from "./CratefieldModulesPane";

const module = (over: Partial<CratefieldModule> = {}): CratefieldModule => ({
  slug: "auth",
  name: "Auth",
  summary: "OAuth2 and JWT authentication",
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
  ...over,
});

describe("filterModules", () => {
  it("matches name, summary, crate_name, slug case-insensitively", () => {
    const modules = [
      module({ slug: "auth", name: "Auth", summary: "OAuth2", crate_name: "cratefield-module-auth" }),
      module({ slug: "payments", name: "Payments", summary: "Stripe integration", crate_name: "cratefield-module-payments" }),
    ];

    expect(filterModules(modules, { query: "auth", status: [], port: [] })).toHaveLength(1);
    expect(filterModules(modules, { query: "AUTH", status: [], port: [] })).toHaveLength(1);
    expect(filterModules(modules, { query: "oauth", status: [], port: [] })).toHaveLength(1);
    expect(filterModules(modules, { query: "stripe", status: [], port: [] })).toHaveLength(1);
    expect(filterModules(modules, { query: "missing", status: [], port: [] })).toHaveLength(0);
  });

  it("filters by status", () => {
    const modules = [
      module({ status: "published" }),
      module({ status: "unreleased" }),
      module({ status: "planned" }),
    ];

    expect(filterModules(modules, { query: "", status: ["published"], port: [] })).toHaveLength(1);
    expect(filterModules(modules, { query: "", status: ["unreleased", "planned"], port: [] })).toHaveLength(2);
    expect(filterModules(modules, { query: "", status: [], port: [] })).toHaveLength(3);
  });

  it("filters by port (needs or optional)", () => {
    const modules = [
      module({ needs: ["Database"], optional: ["Cache"] }),
      module({ needs: ["Email"], optional: [] }),
      module({ needs: [], optional: ["Queue"] }),
    ];

    expect(filterModules(modules, { query: "", status: [], port: ["Database"] })).toHaveLength(1);
    expect(filterModules(modules, { query: "", status: [], port: ["Cache"] })).toHaveLength(1);
    expect(filterModules(modules, { query: "", status: [], port: ["Email"] })).toHaveLength(1);
    expect(filterModules(modules, { query: "", status: [], port: ["Queue"] })).toHaveLength(1);
    expect(filterModules(modules, { query: "", status: [], port: ["Database", "Email"] })).toHaveLength(2);
  });
});

describe("portsOf", () => {
  it("returns sorted union of needs and optional", () => {
    const modules = [
      module({ needs: ["Database", "Email"], optional: ["Cache"] }),
      module({ needs: ["Cache"], optional: ["Queue"] }),
      module({ needs: [], optional: [] }),
    ];

    const ports = portsOf(modules);
    expect(ports).toEqual(["Cache", "Database", "Email", "Queue"]);
  });

  it("returns empty array for empty modules", () => {
    expect(portsOf([])).toEqual([]);
  });
});

describe("catalogueNotice", () => {
  it("returns null when source and registry are both live", () => {
    const notice = catalogueNotice({
      source: "live",
      fetched_at: "2026-10-19T14:32:00Z",
      error: null,
      registry: "live",
      catalogue_url: "https://cratefield.dev/modules",
      modules: [],
    });
    expect(notice).toBeNull();
  });

  it("warns for snapshot with date", () => {
    const notice = catalogueNotice({
      source: "snapshot",
      fetched_at: "2026-10-19T14:32:00Z",
      error: null,
      registry: "live",
      catalogue_url: "https://cratefield.dev/modules",
      modules: [],
    });
    expect(notice).toContain("snapshot");
    expect(notice).toContain("10/19/2026");
  });

  it("includes error when present in snapshot", () => {
    const notice = catalogueNotice({
      source: "snapshot",
      fetched_at: "2026-10-19T14:32:00Z",
      error: "Network timeout",
      registry: "live",
      catalogue_url: "https://cratefield.dev/modules",
      modules: [],
    });
    expect(notice).toContain("Network timeout");
  });

  it("warns for cache with date", () => {
    const notice = catalogueNotice({
      source: "cache",
      fetched_at: "2026-10-19T14:32:00Z",
      error: null,
      registry: "live",
      catalogue_url: "https://cratefield.dev/modules",
      modules: [],
    });
    expect(notice).toContain("copy fetched");
    expect(notice).toContain("10/19/2026");
  });

  it("says unknown date for invalid RFC3339", () => {
    const notice = catalogueNotice({
      source: "snapshot",
      fetched_at: "not-a-date",
      error: null,
      registry: "live",
      catalogue_url: "https://cratefield.dev/modules",
      modules: [],
    });
    expect(notice).toContain("unknown date");
  });

  it("says unknown date when fetched_at is null", () => {
    const notice = catalogueNotice({
      source: "snapshot",
      fetched_at: null,
      error: null,
      registry: "live",
      catalogue_url: "https://cratefield.dev/modules",
      modules: [],
    });
    expect(notice).toContain("unknown date");
  });

  it("notes when registry is not live", () => {
    const notice = catalogueNotice({
      source: "live",
      fetched_at: "2026-10-19T14:32:00Z",
      error: null,
      registry: "cache",
      catalogue_url: "https://cratefield.dev/modules",
      modules: [],
    });
    expect(notice).toContain("Published status comes from a cache copy of crates.io");
  });
});
