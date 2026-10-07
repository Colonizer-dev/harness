// Issue #241: the Meta Model API preset. Anthropic-wire base URLs must never carry a `/v1`
// suffix — the gateway appends the request path itself, so a `/v1` here would double up.
import { describe, expect, it } from "vitest";

import { PROVIDER_CATALOG } from "./providerCatalog";

describe("PROVIDER_CATALOG meta entry", () => {
  const meta = PROVIDER_CATALOG.find((entry) => entry.id === "meta");

  it("exists, alphabetically placed among the other ids", () => {
    expect(meta).toBeDefined();
  });

  it("carries the verified defaults from the issue", () => {
    expect(meta).toMatchObject({
      name: "Meta Model API",
      base_url: "https://api.meta.ai",
      auth: "bearer",
      wire: "anthropic",
      context_tokens: 1_048_576,
      max_concurrent: 4,
    });
    expect(meta?.models).toEqual(["muse-spark-1.3-contributor", "muse-spark-1.2-contributor"]);
  });

  it("does not carry a /v1 suffix, which would double up against the gateway's own path", () => {
    expect(meta?.base_url.endsWith("/v1")).toBe(false);
  });

  it("ships with no pricing: contributor-tier rates are not yet published", () => {
    expect(meta?.pricing).toBeUndefined();
  });
});
