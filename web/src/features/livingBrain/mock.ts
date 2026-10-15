// The mock install's Living Brain (crates/colonizer/src/living_brain): disconnected, like a fresh
// install. Connect walks the device sign-in the way the server does — a code first, approved a few
// seconds later — so `?mock=1` shows every state. Its state is its own; no other feature reads it.
import { clone, now, sleep } from "../../mockShared";
import { ApiError } from "../../http";
import type { LivingBrainApi } from "./api";
import type { LivingBrainState } from "./types";

const fresh = (): LivingBrainState => ({
  state: "disconnected",
  server_url: "https://app.livingbrain.wiki",
  mcp_url: "https://mcp.livingbrain.wiki/v1/pages/mcp",
  connected_at: null,
  token_prefix: null,
  token_name: null,
  auth_error: null,
  device: null,
  rules: [],
  mcp_agents: ["claude-code", "codex"],
  queue: { pending: 0, sent: 0, failed: 0, last_sync: null, last_error: null, items: [] },
});

export function livingBrainMock(): LivingBrainApi {
  let s = fresh();
  const connected = () => {
    s = { ...s, state: "connected", connected_at: now(), token_prefix: "lbp_01JMOCK", token_name: "colonizer", auth_error: null };
  };
  return {
    livingBrain: async () => clone(s),
    saveLivingBrain: async (body) => {
      await sleep(150);
      const moved = (body.server_url && body.server_url !== s.server_url) || (body.mcp_url && body.mcp_url !== s.mcp_url);
      if (moved && s.state !== "disconnected") throw new ApiError("disconnect first: the token stays with the server that issued it", 409);
      if (body.server_url) s.server_url = body.server_url;
      if (body.mcp_url) s.mcp_url = body.mcp_url;
      if (body.rules) {
        s.rules = body.rules.map((r) => {
          const before = s.rules.find((b) => b.target.toLowerCase() === r.target.toLowerCase());
          return { ...r, since: r.send ? (before?.send ? before.since ?? now() : now()) : null };
        });
      }
      return clone(s);
    },
    connectLivingBrain: async () => {
      await sleep(250);
      s.device = {
        state: "pending",
        user_code: "WDJB-MJHT",
        verification_uri: "https://app.livingbrain.wiki/device",
        verification_uri_complete: "https://app.livingbrain.wiki/device?code=WDJB-MJHT",
        expires_at: new Date(Date.now() + 600_000).toISOString(),
      };
      // The demo's person approves after a few seconds.
      setTimeout(() => {
        if (s.device?.state !== "pending") return;
        connected();
        s.device = { ...s.device, state: "connected" };
      }, 4000);
      return clone(s);
    },
    cancelLivingBrainConnect: async () => {
      s.device = null;
      return clone(s);
    },
    setLivingBrainToken: async (token) => {
      await sleep(250);
      if (!token.startsWith("lbp_")) throw new ApiError("Living Brain refused this token (401); create a new one and try again", 400);
      connected();
      s.device = null;
      return clone(s);
    },
    disconnectLivingBrain: async () => {
      await sleep(250);
      const rules = s.rules;
      s = { ...fresh(), server_url: s.server_url, mcp_url: s.mcp_url, rules, revoke_error: null };
      return clone(s);
    },
    retryLivingBrain: async () => {
      const retried = s.queue.failed;
      s.queue = { ...s.queue, pending: s.queue.pending + retried, failed: 0 };
      return { retried, queue: clone(s.queue) };
    },
    backfillLivingBrain: async (org, dryRun) => {
      await sleep(200);
      if (s.state === "disconnected") throw new ApiError("connect Living Brain first", 409);
      const sending = s.rules.some((r) => r.send && r.target.split("/")[0].toLowerCase() === org.toLowerCase());
      const eligible = sending ? 3 : 0;
      if (!dryRun) s.queue = { ...s.queue, pending: s.queue.pending + eligible };
      return { org, dry_run: dryRun, eligible, already_sent: 0, to_send: eligible, queued: dryRun ? 0 : eligible, not_opted_in: sending ? 0 : 3, still_running: 1 };
    },
    livingBrainColony: async () => ({ state: "none", note_url: null, sent_at: null, last_error: null, sessions_url: `${s.server_url}/wiki.html?view=sessions` }),
  };
}
