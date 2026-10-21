// The brain map the mothership proxies for the cockpit's 3D view (GET /api/brain/graph). Structure
// only: a node is an index, a kind, a degree and either its label or, in structure-only mode, a
// hash of it. Nothing else of the brain reaches the cockpit.

export interface BrainMapNode {
  /** The node's index in this answer; links and matches name nodes by it. */
  id: number;
  kind: string;
  degree: number;
  /** The label, when the answer is not structure only. */
  label?: string;
  /** The opaque stand-in for the label, in structure-only mode. */
  hash?: string;
}

export interface BrainMapConnected {
  state: "connected";
  /** Labels are hashes in this answer (the toggle, or a remote request, which always is). */
  structure_only: boolean;
  /** The request came through the remote tunnel; the answer is structure only whatever was asked. */
  remote: boolean;
  fetched_at: string;
  truncated: boolean;
  counts: { nodes: number; links: number };
  nodes: BrainMapNode[];
  /** Index pairs. */
  links: [number, number][];
  /** For each repository asked for: the node it sits on, or null. */
  matches: Record<string, number | null>;
  /** The node the map is centred on when no colony names one. */
  nest: number | null;
}

export type BrainMapAnswer = { state: "disconnected" } | { state: "unauthorized" } | BrainMapConnected;
