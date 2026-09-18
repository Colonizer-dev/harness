// meshBroken is the one gate behind the sidebar dot, the Settings nav and the Runtime row. A Mac
// vendors no tailscaled, so `state: "unavailable"` with a detail is by design, not a fault (#128);
// these tests pin that it never reads as an error, and that real failures still do.
import { describe, expect, it } from "vitest";

import { meshBroken } from "./components/ui";
import type { HarnessStatus } from "./types";

type Mesh = NonNullable<HarnessStatus["mesh"]>;

// The three payloads the mock serves for /api/status (web/src/mock.ts), so the fixtures track what
// the wire really carries.
const MESH_ERROR: Mesh = {
  enabled: true,
  provider: "headscale",
  state: "error",
  harness_ip: null,
  nodes: 0,
  error: "headscale did not start: address already in use",
};
const MESH_UNAVAILABLE: Mesh = {
  enabled: true,
  provider: "headscale",
  state: "unavailable",
  harness_ip: null,
  nodes: 0,
  detail: "colonies use a loopback port on this platform",
  error: null,
};
const MESH_RUNNING: Mesh = {
  enabled: true,
  provider: "headscale",
  state: "running",
  harness_ip: "100.64.0.1",
  nodes: 3,
  error: null,
};

describe("meshBroken", () => {
  it("reads an unavailable mesh as by design, not a fault", () => {
    // The Mac regression: this payload used to put its explanation in `error` and painted red.
    expect(meshBroken(MESH_UNAVAILABLE)).toBe(false);
  });

  it("keeps an unavailable mesh non-broken even when a stale error tags along", () => {
    // The unavailable check wins over `error`, so an old failure message cannot redden a platform
    // that has no mesh to fail.
    expect(meshBroken({ ...MESH_UNAVAILABLE, error: MESH_ERROR.error })).toBe(false);
  });

  it("reads a real mesh failure as broken", () => {
    expect(meshBroken(MESH_ERROR)).toBe(true);
  });

  it("reads a truthy error as broken when no state names the condition", () => {
    expect(meshBroken({ enabled: true, error: MESH_ERROR.error })).toBe(true);
  });

  it("is never broken while the mesh is switched off", () => {
    expect(meshBroken({ ...MESH_ERROR, enabled: false })).toBe(false);
  });

  it("is never broken when there is no mesh to speak of", () => {
    expect(meshBroken(null)).toBe(false);
    expect(meshBroken(undefined)).toBe(false);
  });

  it("reads a healthy running mesh as not broken", () => {
    expect(meshBroken(MESH_RUNNING)).toBe(false);
  });
});
