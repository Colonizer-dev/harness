import { describe, expect, it } from "vitest";
import type { Session } from "../../features/sessions/types";
import type { BrainMapConnected } from "../../features/brainMap/types";
import {
  BRAIN_AXES,
  DEFAULT_CAMERA,
  MAX_ANTS,
  adjacency,
  antPlan,
  antWorld,
  hashString,
  isAntStatus,
  layoutBrain,
  nearest,
  nodeRadius,
  projectPoint,
  type Ant,
} from "./scene";

const ring = (n: number): [number, number][] => Array.from({ length: n }, (_, i) => [i, (i + 1) % n] as [number, number]);

function session(id: string, status: Session["status"], repo: string, extra: Partial<Session> = {}): Session {
  return { id, repo, status, issue: 1, issue_title: `Fix ${id}`, ...extra } as Session;
}

function answer(matches: Record<string, number | null>, nest: number | null = 0): BrainMapConnected {
  return {
    state: "connected",
    structure_only: false,
    remote: false,
    fetched_at: "2026-10-21T00:00:00Z",
    truncated: false,
    counts: { nodes: 4, links: 4 },
    nodes: [0, 1, 2, 3].map((id) => ({ id, kind: "topic", degree: 2, label: `n${id}` })),
    links: ring(4),
    matches,
    nest,
  };
}

describe("layoutBrain", () => {
  it("is deterministic and keeps every node inside the brain's ellipsoid", () => {
    const links = ring(300);
    const a = layoutBrain(300, links);
    const b = layoutBrain(300, links);
    expect(a.length).toBe(900);
    expect(Array.from(a)).toEqual(Array.from(b));
    for (let i = 0; i < 300; i += 1) {
      const x = a[i * 3] / BRAIN_AXES.x;
      const y = a[i * 3 + 1] / BRAIN_AXES.y;
      const z = a[i * 3 + 2] / BRAIN_AXES.z;
      expect(Math.sqrt(x * x + y * y + z * z)).toBeLessThanOrEqual(0.98 + 1e-6);
    }
  });

  it("pulls linked nodes closer together than unlinked ones", () => {
    const n = 120;
    const links = ring(n);
    const pos = layoutBrain(n, links);
    const dist = (a: number, b: number) =>
      Math.hypot(pos[a * 3] - pos[b * 3], pos[a * 3 + 1] - pos[b * 3 + 1], pos[a * 3 + 2] - pos[b * 3 + 2]);
    let linked = 0;
    let far = 0;
    for (let i = 0; i < n; i += 1) {
      linked += dist(i, (i + 1) % n);
      far += dist(i, (i + n / 2) % n);
    }
    expect(linked / n).toBeLessThan(far / n);
  });

  it("handles an empty and a single-node brain", () => {
    expect(layoutBrain(0, []).length).toBe(0);
    expect(layoutBrain(1, []).length).toBe(3);
  });
});

describe("projectPoint and picking", () => {
  it("projects the origin to the middle of the canvas, and zoom moves points away from it", () => {
    const at = projectPoint(0, 0, 0, DEFAULT_CAMERA, 400, 300);
    expect(at.x).toBeCloseTo(200);
    expect(at.y).toBeCloseTo(150);
    const near = projectPoint(0.8, 0, 0, { ...DEFAULT_CAMERA, yaw: 0, pitch: 0, zoom: 1 }, 400, 300);
    const zoomed = projectPoint(0.8, 0, 0, { yaw: 0, pitch: 0, zoom: 2 }, 400, 300);
    expect(zoomed.x - 200).toBeGreaterThan(near.x - 200);
  });

  it("gives a farther point a smaller scale", () => {
    const camera = { yaw: 0, pitch: 0, zoom: 1 };
    const front = projectPoint(0, 0, -1, camera, 400, 300);
    const back = projectPoint(0, 0, 1, camera, 400, 300);
    expect(front.scale).toBeGreaterThan(back.scale);
    expect(front.depth).toBeLessThan(back.depth);
  });

  it("sizes busier nodes larger within bounds", () => {
    expect(nodeRadius(50, 50, 1)).toBeGreaterThan(nodeRadius(1, 50, 1));
    expect(nodeRadius(0, 0, 1)).toBeGreaterThanOrEqual(1.2);
    expect(nodeRadius(1000, 1000, 10)).toBeLessThanOrEqual(9);
  });

  it("picks the nearest item within the slop, and nothing beyond it", () => {
    const items = [
      { x: 10, y: 10, r: 2, name: "a" },
      { x: 30, y: 10, r: 2, name: "b" },
    ];
    expect(nearest(items, 12, 10, 4)?.name).toBe("a");
    expect(nearest(items, 100, 100, 4)).toBeNull();
  });
});

describe("antPlan", () => {
  it("draws the colonies still in play as ants, on the node their repository matched", () => {
    const sessions = [
      session("c1", "running", "acme/web"),
      session("c2", "queued", "acme/api"),
      session("c3", "merged", "acme/web"),
      session("c4", "waiting_for_answer", "acme/unknown"),
      session("c5", "idle", "acme/web", { summary: "Tidy the header" }),
    ];
    const ants = antPlan(sessions, answer({ "acme/web": 2, "acme/api": 1, "acme/unknown": null }, 3));
    expect(ants.map((a) => a.id)).toEqual(["c1", "c2", "c4", "c5"]);
    expect(ants.find((a) => a.id === "c1")).toMatchObject({ node: 2, tone: "info", moving: true });
    // An unmatched repository sits at the nest.
    expect(ants.find((a) => a.id === "c4")).toMatchObject({ node: 3, tone: "accent", moving: false });
    expect(ants.find((a) => a.id === "c5")?.title).toBe("Tidy the header");
  });

  it("holds a suspended colony still, and gives no ant to a finished one", () => {
    const suspended = session("s1", "running", "acme/web", { suspended: { at: "2026-10-21T00:00:00Z" } as never });
    const [ant] = antPlan([suspended], answer({ "acme/web": 0 }));
    expect(ant.moving).toBe(false);
    expect(isAntStatus("failed")).toBe(false);
    expect(isAntStatus("parked")).toBe(true);
  });

  it("caps the ants, and draws none when there is no node at all", () => {
    const many = Array.from({ length: MAX_ANTS + 20 }, (_, i) => session(`x${i}`, "running", "acme/web"));
    expect(antPlan(many, answer({ "acme/web": 0 })).length).toBe(MAX_ANTS);
    expect(antPlan([session("n", "running", "acme/web")], answer({}, null))).toEqual([]);
  });
});

describe("antWorld", () => {
  const positions = Float32Array.from([0, 0, 0, 1, 0, 0, 0, 1, 0]);
  const neighbours = adjacency(3, [[0, 1], [0, 2]]);
  const ant = (over: Partial<Ant>): Ant => ({
    id: "a",
    title: "t",
    repo: "o/r",
    status: "running",
    node: 0,
    tone: "info",
    moving: true,
    seed: hashString("a"),
    ...over,
  });

  it("keeps a still ant beside its node", () => {
    const out = new Float32Array(3);
    antWorld(ant({ moving: false }), 5, positions, neighbours, out, 0);
    expect(Math.hypot(out[0], out[1], out[2])).toBeLessThan(0.05);
  });

  it("walks a moving ant along an edge from its node", () => {
    const out = new Float32Array(3);
    for (const t of [0, 1.3, 2.7, 9]) {
      antWorld(ant({}), t, positions, neighbours, out, 0);
      expect(out[0]).toBeGreaterThanOrEqual(-0.05);
      expect(out[0]).toBeLessThanOrEqual(1.05);
      expect(Math.abs(out[1])).toBeLessThan(0.05);
    }
  });

  it("keeps the adjacency symmetric", () => {
    expect(neighbours[1]).toEqual([0]);
    expect(neighbours[0].sort()).toEqual([1, 2]);
  });
});
