// The fleet panel's whole point is that a stalled host must look obviously different from a
// healthy idle one (issue #231's core ask), and that a single-machine install — still the common
// case — sees no new panel at all. Rendered through react-dom/server, matching this codebase's
// other cockpit tests: no jsdom, assertions read the markup string directly.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { FleetPanel } from "./FleetPanel";
import type { FleetHost } from "../types";

const SELF: FleetHost = {
  id: "a6b09e90-e553-43ce-a6c3-ab815bb14d34",
  name: "colonizer-3a7c8112",
  platform: "linux-x86_64",
  os: "Debian GNU/Linux",
  version: "0.1.5",
  slots_in_use: 0,
  slots_ceiling: 3,
  queue_depth: 0,
  disk_free_bytes: 534_179_840,
  last_heartbeat: "2026-09-21T04:31:25Z",
  health: "online",
};

const PEER_DOWN: FleetHost = {
  id: "peer-1",
  name: "https://peer.example:9443",
  platform: "linux-x86_64",
  os: "Ubuntu",
  version: "0.1.4",
  slots_in_use: 1,
  slots_ceiling: 2,
  queue_depth: 0,
  disk_free_bytes: 1_073_741_824,
  last_heartbeat: "2026-09-21T02:00:00Z",
  health: "unreachable",
};

describe("FleetPanel", () => {
  it("renders nothing for a single-machine install", () => {
    expect(renderToStaticMarkup(<FleetPanel hosts={[SELF]} />)).toBe("");
  });

  it("renders nothing with no hosts at all", () => {
    expect(renderToStaticMarkup(<FleetPanel hosts={[]} />)).toBe("");
  });

  it("lists every host once there is more than one", () => {
    const markup = renderToStaticMarkup(<FleetPanel hosts={[SELF, PEER_DOWN]} />);
    expect(markup).toContain("colonizer-3a7c8112");
    expect(markup).toContain("https://peer.example:9443");
    expect(markup).toContain("2 hosts");
  });

  it("makes an unreachable host look obviously different from an online one", () => {
    const markup = renderToStaticMarkup(<FleetPanel hosts={[SELF, PEER_DOWN]} />);
    expect(markup).toContain("bg-ok");
    expect(markup).toContain("bg-err");
    expect(markup).toContain("online");
    expect(markup).toContain("unreachable");
  });

  it("still shows a never-reached peer's placeholder row without crashing", () => {
    const placeholder: FleetHost = {
      id: "https://ghost.example",
      name: "https://ghost.example",
      platform: "",
      os: "",
      version: null,
      slots_in_use: 0,
      slots_ceiling: 0,
      queue_depth: 0,
      disk_free_bytes: null,
      last_heartbeat: null,
      health: "unreachable",
    };
    const markup = renderToStaticMarkup(<FleetPanel hosts={[SELF, placeholder]} />);
    expect(markup).toContain("https://ghost.example");
    expect(markup).toContain("never");
    expect(markup).toContain("bg-err");
  });
});
