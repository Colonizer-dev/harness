// GET /api/hosts (issue #231): self plus every configured peer, polled live on each request. A
// peer never drops out of the list — an unreachable one keeps its last-known stats, and one never
// reached at all comes back as a bare placeholder. These pure derivations must read both shapes
// without drawing a number the host never measured, exactly like host.ts does for the self host.
import { describe, expect, it } from "vitest";

import { fleetHostFacts, timeSinceHeartbeat } from "./fleet";
import type { FleetHost } from "../types";

const SELF: FleetHost = {
  id: "a6b09e90-e553-43ce-a6c3-ab815bb14d34",
  name: "colonizer-3a7c8112",
  platform: "linux-x86_64",
  os: "Debian GNU/Linux",
  version: "0.1.5",
  slots_in_use: 1,
  slots_ceiling: 3,
  queue_depth: 0,
  disk_free_bytes: 534_179_840, // ~509M
  last_heartbeat: "2026-09-21T04:31:25.635166368+00:00",
  health: "online",
};

describe("fleetHostFacts", () => {
  it("lists slots, disk free and version for a healthy host with no queue", () => {
    expect(fleetHostFacts(SELF).map((f) => f.value)).toEqual(["1/3", "disk 509.4M free", "0.1.5"]);
  });

  it("shows the queue depth only when work is actually waiting", () => {
    const queued: FleetHost = { ...SELF, queue_depth: 4 };
    expect(fleetHostFacts(queued).map((f) => f.value)).toEqual(["1/3", "4 queued", "disk 509.4M free", "0.1.5"]);
  });

  it("carries a peer's last-known stats while it is unreachable", () => {
    const cached: FleetHost = {
      id: "peer-1",
      name: "https://peer.example:9443",
      platform: "linux-x86_64",
      os: "Ubuntu",
      version: "0.1.4",
      slots_in_use: 2,
      slots_ceiling: 4,
      queue_depth: 1,
      disk_free_bytes: 1_073_741_824, // 1G
      last_heartbeat: "2026-09-21T03:00:00Z",
      health: "unreachable",
    };
    expect(fleetHostFacts(cached).map((f) => f.value)).toEqual(["2/4", "1 queued", "disk 1G free", "0.1.4"]);
  });

  it("drops every segment a never-reached peer has no data for", () => {
    const placeholder: FleetHost = {
      id: "https://peer.example:9443",
      name: "https://peer.example:9443",
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
    expect(fleetHostFacts(placeholder).map((f) => f.value)).toEqual(["0/0"]);
  });
});

describe("timeSinceHeartbeat", () => {
  const now = new Date("2026-09-21T04:32:00Z");

  it("reads a recent heartbeat as just now", () => {
    expect(timeSinceHeartbeat("2026-09-21T04:31:25Z", now)).toBe("just now");
  });

  it("reads an older heartbeat in minutes", () => {
    expect(timeSinceHeartbeat("2026-09-21T04:00:00Z", now)).toBe("32m ago");
  });

  it("reads a never-reached peer as never, not a bogus duration", () => {
    expect(timeSinceHeartbeat(null, now)).toBe("never");
  });
});
