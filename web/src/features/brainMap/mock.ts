// The mock install's brain map (`?mock=1`): a fixture graph shaped like the mothership's answer,
// with the mock sessions' repositories as project nodes, so the colonies walk the brain. The same
// structure-only rules apply: a structure toggle hashes the labels.
import type { BrainMapApi, BrainMapRequest } from "./api";
import type { BrainMapConnected, BrainMapNode } from "./types";

const PROJECTS = ["acme/webshop", "acme/api", "acme/design-system", "octocat/hello-world"];
const TOPICS = ["Checkout flow", "Refund policy", "Auth tokens", "Release train", "Onboarding", "Pricing notes", "Inventory sync", "Support macros"];
const KINDS = ["decision", "person", "system", "topic", "session", "source", "fact"];

interface Raw {
  label: string;
  kind: string;
  degree: number;
  links: number[];
}

/** A deterministic fixture: projects with their pages, linked to each other and a few shared topics. */
function fixture(): Raw[] {
  const nodes: Raw[] = [];
  PROJECTS.forEach((name) => nodes.push({ label: name.split("/")[1], kind: "project", degree: 0, links: [] }));
  TOPICS.forEach((label, i) => nodes.push({ label, kind: KINDS[i % KINDS.length], degree: 0, links: [] }));
  for (let i = 0; i < 26; i += 1) nodes.push({ label: `Page ${i + 1}`, kind: KINDS[i % KINDS.length], degree: 0, links: [] });
  const link = (a: number, b: number) => {
    if (a === b || nodes[a].links.includes(b)) return;
    nodes[a].links.push(b);
    nodes[b].links.push(a);
  };
  for (let i = 0; i < nodes.length; i += 1) {
    link(i, (i * 7 + 3) % nodes.length);
    if (i > 0) link(i, Math.floor(i / 2));
  }
  nodes.forEach((n) => (n.degree = n.links.length));
  return nodes;
}

const FIXTURE = fixture();

function matchRepo(repo: string): number | null {
  const [owner, name] = repo.toLowerCase().split("/");
  let best: number | null = null;
  let bestScore = 0;
  FIXTURE.forEach((node, at) => {
    const label = node.label.toLowerCase();
    const score = label === repo.toLowerCase() ? 3 : label === name ? 2 : label === owner ? 1 : 0;
    if (score > bestScore) {
      best = at;
      bestScore = score;
    }
  });
  return best;
}

export const brainMapMock = (): BrainMapApi => ({
  brainMap: async (body: BrainMapRequest = {}) => {
    const structure = Boolean(body.structure);
    const nodes: BrainMapNode[] = FIXTURE.map((node, id) =>
      structure
        ? { id, kind: node.kind, degree: node.degree, hash: `mock${id.toString(16).padStart(12, "0")}` }
        : { id, kind: node.kind, degree: node.degree, label: node.label },
    );
    const links: [number, number][] = [];
    FIXTURE.forEach((node, a) => node.links.forEach((b) => a < b && links.push([a, b])));
    const matches: Record<string, number | null> = {};
    for (const repo of body.repos ?? []) matches[repo] = matchRepo(repo);
    let nest: number | null = null;
    FIXTURE.forEach((node, at) => {
      if (nest == null || node.degree > FIXTURE[nest].degree) nest = at;
    });
    const answer: BrainMapConnected = {
      state: "connected",
      structure_only: structure,
      remote: false,
      fetched_at: new Date().toISOString(),
      truncated: false,
      counts: { nodes: nodes.length, links: links.length },
      nodes,
      links,
      matches,
      nest,
    };
    return answer;
  },
});
