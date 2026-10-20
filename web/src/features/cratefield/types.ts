// Cratefield modules catalogue (issue #1291): modules available from the Cratefield registry.
// Split out of src/types.ts with the rest of the per-feature types.

/** GET /api/cratefield/catalog (issue #1291): the Cratefield modules catalogue. */
export interface CratefieldCatalog {
  /** Whether the data comes from live registry, a local cache, or a bundled snapshot. */
  source: "live" | "cache" | "snapshot";
  /** The registry copy's publication date (RFC3339), or null if unavailable. */
  fetched_at: string | null;
  /** Any error from the last fetch attempt, or null. */
  error: string | null;
  /** The source of the published/unreleased/planned status: live registry or a cached/snapshot copy. */
  registry: "live" | "cache" | "snapshot";
  /** The catalogue URL modules were read from. */
  catalogue_url: string;
  /** Every known Cratefield module. */
  modules: CratefieldModule[];
}

/** GET /api/cratefield/catalog `modules[]` (issue #1291): one Cratefield module. */
export interface CratefieldModule {
  /** The module's slug in the catalogue (e.g. `waitlist`). */
  slug: string;
  /** The module's human-readable name. */
  name: string;
  /** A one-sentence description. */
  summary: string;
  /** The Rust crate name (e.g. `cratefield-module-waitlist`). */
  crate_name: string;
  /** The version in the current build, or null if not determined. */
  version: string | null;
  /** The latest version on crates.io (the canonical version), or null if not determined. */
  registry_version: string | null;
  /** `published` for a release, `unreleased` for a version on main, `planned` for something named but not yet coded. */
  status: "published" | "unreleased" | "planned";
  /** Port names this module requires. */
  needs: string[];
  /** Port names this module can use if available. */
  optional: string[];
  /** Module slugs this module depends on. */
  depends_on: string[];
  /** Database tables this module creates or uses. */
  tables: string[];
  /** Links: docs.rs, crates.io, and the catalogue entry. */
  links: {
    docs_rs: string | null;
    crate: string | null;
    catalogue: string | null;
  };
}
