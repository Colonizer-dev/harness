// Living Brain (crates/colonizer/src/living_brain): the connection, the per-org/per-repo rules and
// the send queue, as GET /api/living-brain answers them. The token itself is never in any answer.

/** How much of a colony travels (the brain CLI's levels): `full` every message, reasoning and tool input/output; `messages` every message, tool names only; `summary` messages cut to 2 000 characters, tool names only. */
export type LivingBrainDetail = "full" | "messages" | "summary";

/** One rule: an org (`acme`) or a repository (`acme/web`); a repository's rule wins over its org's. No rule is off. */
export interface LivingBrainRule {
  target: string;
  send: boolean;
  detail: LivingBrainDetail;
  mcp: boolean;
  /** RFC3339, set by the server when `send` was switched on: only colonies active since go out on their own. */
  since?: string | null;
}

/** The device sign-in in flight (RFC 8628): the code to show and the page to open. */
export interface LivingBrainDevice {
  state: "pending" | "connected" | "denied" | "expired" | "failed";
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string | null;
  expires_at: string;
  error?: string;
}

export interface LivingBrainQueueItem {
  colony: string;
  repo: string;
  state: "pending" | "sent" | "failed";
  reason: string;
  attempts: number;
  last_error: string | null;
  queued_at: string | null;
  sent_at: string | null;
  note_url: string | null;
}

export interface LivingBrainQueue {
  pending: number;
  sent: number;
  failed: number;
  last_sync: string | null;
  last_error: string | null;
  items: LivingBrainQueueItem[];
}

export interface LivingBrainState {
  /** `unauthorized`: the brain refused the saved token (revoked or expired) — reconnect. */
  state: "disconnected" | "connected" | "unauthorized";
  server_url: string;
  mcp_url: string;
  connected_at: string | null;
  /** The token's public half, `lbp_<id>`. */
  token_prefix: string | null;
  /** The token's name at the brain, when it lists it. */
  token_name: string | null;
  auth_error: string | null;
  device: LivingBrainDevice | null;
  rules: LivingBrainRule[];
  /** The agent modules that can mount the brain's MCP server. */
  mcp_agents: string[];
  queue: LivingBrainQueue;
  /** DELETE /api/living-brain/connection only: the brain could not be told to revoke the token. */
  revoke_error?: string | null;
}

export interface LivingBrainBackfill {
  org: string;
  dry_run: boolean;
  /** Settled colonies of the org in repositories whose rule sends. */
  eligible: number;
  already_sent: number;
  to_send: number;
  queued: number;
  not_opted_in: number;
  still_running: number;
}

export interface LivingBrainColony {
  state: "none" | "pending" | "sent" | "failed";
  note_url: string | null;
  sent_at: string | null;
  last_error: string | null;
  sessions_url: string;
}
