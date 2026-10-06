// The "Built with" list (issue #944): the venture's own stack, vendored into the mothership so the
// cockpit can show it without reaching the network. Split out of src/types.ts with the rest of the
// per-feature types.

/** GET /api/built-with (issue #944): what this venture is built with, and where that claim is published. */
export interface BuiltWith {
  /** The stack registry this list was taken from — the canonical, machine-readable copy. */
  registry: string;
  /** The venture's id in that registry, e.g. `FZ-006`. */
  venture: string;
  /** The venture's name as the registry spells it. */
  venture_name: string;
  /** The registry's human-readable page for this venture: the provenance a person can read. */
  venture_page: string;
  /** The day the vendored copy was taken (YYYY-MM-DD); null when the mothership read it live and has no vendored copy to name. */
  retrieved: string | null;
  /** One entry per product, live or planned. Empty is a valid answer, not a fault. */
  uses: BuiltWithUse[];
}

/** GET /api/built-with `uses[]` (issue #944): one product this venture is built with, or plans to be. */
export interface BuiltWithUse {
  /** The product's id in the registry (`FZ-004`), or its vendor's own slug for a third-party product (`polar`). */
  id: string;
  /** `factory-zero` for another Factory Zero venture, `third-party` for a product bought in. */
  kind: string;
  /** The product's name, as the registry spells it. */
  name: string;
  /** What this venture gets from it, in one sentence — the line that makes the entry worth reading. */
  note: string;
  /** The label the entry is filed under, e.g. "Email by", "Payments by": the registry's own wording, not ours. */
  phrase: string;
  /** The job the product does, e.g. `email`, `payments`, `hosting`. */
  role: string;
  /**
   * Whether the product is in use today (`live`) or named for later (`planned`). Only these two
   * values: the cockpit shows the word, and a planned entry is never dressed as a live one.
   */
  status: "live" | "planned";
  /** The product's own page. */
  url: string;
}
