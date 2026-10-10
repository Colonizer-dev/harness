// The `remote` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { clone, now, sleep } from "../../mockShared";
import { defaultPushPrefs, mergePushPrefs } from "../../push";
import type { ApiTokenMeta, CratefieldPushState, PhoneOrigin, PushSubscriptionSummary } from "../../types";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { RemoteApi } from "./api";

// Where a phone might reach this cockpit, in the mothership's preference order: the relay when
// remote access is on, otherwise a plain-http lan address (so the insecure-origin warning has a
// real case). GET /api/phone answers with this list too — bare origins, no code — so a bookmark
// can name the network address without minting an invite (issue #867).
const phoneOrigins = (ms: MockState): PhoneOrigin[] =>
  ms.remoteState.enabled
    ? [{ kind: "relay", url: `https://${ms.remoteHost}`, reachable: true, secure: true, note: null }]
    : [{ kind: "lan", url: "http://192.168.1.20:7878", reachable: true, secure: false, note: "Plain http: prefer the relay link" }];

// The server derives `state` on every read (cratefield_push.rs `view`): off while the switch is,
// no_remote when remote access has no link to sign with, then queued or unreachable from the
// queue, ok once it drains. The mock keeps the bookkeeping and derives the same way.
const cratefieldView = (ms: MockState): CratefieldPushState => {
  const s = ms.cratefieldState;
  const state = !s.enabled ? "off" : !ms.remoteState.enabled ? "no_remote" : s.queued > 0 ? (s.last_error ? "unreachable" : "queued") : "ok";
  return { ...s, state };
};

export function remoteMock(ms: MockState): RemoteApi {
  return {
    pushKey: () => ms.later(() => ({ public_key: ms.MOCK_PUSH_KEY })),
    pushSubscriptions: () => ms.later(() => [...ms.pushSubs].sort((a, b) => b.created_at - a.created_at)),
    subscribePush: async (body) => {
      await sleep(300);
      // The server answers 400 with a message for anything short of a full subscription; a blank
      // label becomes the default, and the endpoint host is all the list ever shows of it.
      const endpoint = (() => {
        try {
          return new URL(body?.endpoint ?? "").host;
        } catch {
          return null;
        }
      })();
      if (!body || !endpoint || !body.keys?.p256dh || !body.keys?.auth) {
        throw new ApiError("the subscription needs an endpoint and its p256dh and auth keys", 400);
      }
      const row: PushSubscriptionSummary = {
        id: `push_${ms.mockId()}`,
        label: ms.pushLabel(body.label),
        created_at: Math.floor(Date.now() / 1000),
        endpoint_host: endpoint,
        last_seen: null,
        prefs: defaultPushPrefs(),
      };
      ms.pushSubs.push(row);
      return clone(row);
    },
    deletePushSubscription: async (id) => {
      await sleep(200);
      const at = ms.pushSubs.findIndex((row) => row.id === id);
      if (at >= 0) ms.pushSubs.splice(at, 1);
    },
    updatePushSubscription: async (id, body) => {
      await sleep(200);
      const row = ms.pushSubs.find((candidate) => candidate.id === id);
      if (!row) throw new ApiError("no such subscription", 404);
      if (body.prefs !== undefined) {
        // Mirrors Prefs::validate (crates/colonizer/src/push.rs): the prefs arrive wholesale and
        // are checked as a whole before any is kept.
        const { events, scope, quiet, utc_offset, tz } = body.prefs;
        const bad = (why: string) => new ApiError(why, 400);
        if (events && Object.keys(events).some((event) => !(event in defaultPushPrefs().events))) throw bad("unknown event in prefs");
        if (scope && (scope.length > 50 || scope.some((entry) => !entry || entry.length > 200 || /\s/.test(entry) || entry.split("/").length > 2))) {
          throw bad("a scope entry is an org or an org/repo: 1..=200 characters, no whitespace, one slash at most");
        }
        if (
          quiet &&
          (!Number.isInteger(quiet.start) || !Number.isInteger(quiet.end) || quiet.start === quiet.end || quiet.start < 0 || quiet.end < 0 || quiet.start > 1439 || quiet.end > 1439)
        ) {
          throw bad("quiet hours are two different minutes since midnight, 0..1440");
        }
        if (typeof utc_offset === "number" && Math.abs(utc_offset) > 840) throw bad("the utc offset is more than 840 minutes");
        if (typeof tz === "string" && tz.length > 64) throw bad("the timezone name is more than 64 characters");
        row.prefs = mergePushPrefs(body.prefs);
      }
      if (body.label !== undefined) row.label = ms.pushLabel(body.label);
      return clone(row);
    },
    testPushSubscription: async (id) => {
      await sleep(250);
      if (!ms.pushSubs.some((candidate) => candidate.id === id)) throw new ApiError("no such device", 404);
      return { sent: true };
    },
    pushPresence: async (body) => {
      // The endpoint arrives whole; the list only keeps its host, so match on that like the server.
      const host = (() => {
        try {
          return new URL(body?.endpoint ?? "").host;
        } catch {
          return null;
        }
      })();
      const row = ms.pushSubs.find((candidate) => candidate.endpoint_host === host);
      if (!row) throw new ApiError("this endpoint is not subscribed", 404);
      row.last_seen = Math.floor(Date.now() / 1000);
    },
    cratefieldPush: () => ms.later(() => cratefieldView(ms)),
    setCratefieldPush: async (enabled) => {
      await sleep(250);
      // Like the server: a PUT that does not change the switch answers the view and records nothing,
      // and enabling without the remote link is the same 409, in the server's words.
      if (enabled === ms.cratefieldState.enabled) return clone(cratefieldView(ms));
      if (enabled && !ms.remoteState.enabled) throw new ApiError("remote access has no link yet; switch remote access on first", 409);
      ms.cratefieldState = enabled
        ? { ...ms.cratefieldState, enabled: true, since: now(), last_error: null }
        : // Like the server's disable: a fresh queue, with the relay's refusal — if the DELETE had
          // one — written back into last_error so a failed cleanup stays on the record. The demo's
          // relay always answers, so there is never a refusal to keep.
          { enabled: false, since: null, state: "off", queued: 0, dropped: 0, last_error: null, last_delivered: null, last_attempt: null };
      ms.logActivity({ kind: "settings.save", actor: "you", via: "cockpit", target: "the Cratefield delivery", section: "notifications" });
      return clone(cratefieldView(ms));
    },
    testCratefieldPush: async () => {
      await sleep(250);
      // The server's 409 for a test with the channel off.
      if (!ms.cratefieldState.enabled) throw new ApiError("Deliver through Cratefield is off; switch it on first", 409);
      // One test notification queued and flushed at once. With the link up the relay answers: the
      // batch drains, both timestamps stamp, the state reads ok. Without it the flush fails like
      // the server's — the batch stays queued, the refusal lands in last_error, and the state
      // reads unreachable until a delivery clears it.
      ms.cratefieldState = ms.remoteState.enabled
        ? { ...ms.cratefieldState, queued: 0, last_error: null, last_attempt: now(), last_delivered: now() }
        : { ...ms.cratefieldState, queued: ms.cratefieldState.queued + 1, last_error: "remote access is off, so the relay cannot be reached", last_attempt: now() };
      return clone(cratefieldView(ms));
    },
    remote: () => ms.later(() => ms.remoteState),
    setRemote: async (enabled) => {
      await sleep(250);
      // Like the server: a PUT that does not change the switch answers the view and records nothing.
      if (enabled === ms.remoteState.enabled) return clone(ms.remoteState);
      ms.remoteState = enabled
        ? { ...ms.remoteState, enabled: true, host: ms.remoteHost, connected: true, since: now(), replaced: false }
        : { ...ms.remoteState, enabled: false, connected: false, since: null };
      ms.logActivity({ kind: enabled ? "remote.enable" : "remote.disable", actor: "you", via: "cockpit", target: "remote access", section: "remote" });
      return clone(ms.remoteState);
    },
    resetRemote: async () => {
      await sleep(300);
      ms.remoteHost = `${ms.remoteInstallId()}.my.colonizer.dev`;
      // A reset redials at once when the switch was on; the host comes back new either way.
      ms.remoteState = { ...ms.remoteState, host: ms.remoteHost, replaced: false, ...(ms.remoteState.enabled ? { connected: true, since: now() } : {}) };
      // A reset also unbinds the old link's owner; the new install starts unowned.
      ms.remotePairingState.owner = null;
      ms.remotePairingState.pending = [];
      // And it rotates the link credentials: every browser signed in to the link is signed out.
      ms.linkState = { devices: [], pending: [] };
      ms.logActivity({ kind: "remote.reset", actor: "you", via: "cockpit", target: "remote access", section: "remote" });
      return clone(ms.remoteState);
    },
    setRemoteRequireGithub: async (requireGithub) => {
      await sleep(250);
      // Like the server: no change answers the view and records nothing.
      if (requireGithub === ms.remoteState.require_github) return clone(ms.remoteState);
      ms.remoteState = { ...ms.remoteState, require_github: requireGithub };
      ms.logActivity({ kind: "remote.require_github", actor: "you", via: "cockpit", target: requireGithub ? "on" : "off", section: "remote" });
      return clone(ms.remoteState);
    },
    remotePairing: () => ms.later(() => ms.remotePairingState),
    confirmRemotePairing: async (code) => {
      await sleep(250);
      // The relay's rules in the relay's order (worker.js confirmPairing): 400 unless six digits,
      // unknown/expired/used is one 404, and only a live code can meet the bound-owner 409 — a
      // confirm deletes every pending pairing, so a used code is gone, never a 409.
      if (!/^\d{6}$/.test(code)) throw new ApiError("code must be 6 digits", 400);
      const at = ms.remotePairingState.pending.findIndex((row) => row.code === code && row.expires_at > Math.floor(Date.now() / 1000));
      if (at < 0) throw new ApiError("no such pairing", 404);
      if (ms.remotePairingState.owner) throw new ApiError("owner already bound", 409);
      const login = ms.remotePairingState.pending[at].github_login;
      ms.remotePairingState.owner = { github_login: login };
      ms.remotePairingState.pending = [];
      ms.logActivity({ kind: "remote.pair", actor: "you", via: "cockpit", target: `@${login}`, section: "remote" });
      return clone({ owner: { github_login: login } });
    },
    rejectRemotePairing: async (code) => {
      await sleep(250);
      // worker.js rejectPairing: 400 unless six digits, 404 unless that code is pending and live.
      if (!/^\d{6}$/.test(code)) throw new ApiError("code must be 6 digits", 400);
      const at = ms.remotePairingState.pending.findIndex((row) => row.code === code && row.expires_at > Math.floor(Date.now() / 1000));
      if (at < 0) throw new ApiError("no such pairing", 404);
      const [row] = ms.remotePairingState.pending.splice(at, 1);
      ms.logActivity({ kind: "remote.pair_reject", actor: "you", via: "cockpit", target: `@${row.github_login}`, section: "remote" });
      return { github_login: row.github_login };
    },
    unbindRemoteOwner: async () => {
      await sleep(250);
      ms.remotePairingState.owner = null;
      ms.remotePairingState.pending = [];
      ms.logActivity({ kind: "remote.unpair", actor: "you", via: "cockpit", target: "remote access", section: "remote" });
    },
    linkDevices: () => ms.later(() => clone(ms.linkState)),
    linkInvite: async () => {
      await sleep(250);
      if (!ms.remoteState.enabled || !ms.remoteState.host) throw new ApiError("switch remote access on first", 409);
      // The demo's other browser opens the link at once and shows 123 456.
      ms.linkState.pending = [{ id: `ph_${ms.mockId()}`, label: "Browser", expires_at: new Date(Date.now() + 5 * 60_000).toISOString() }];
      return clone({ url: `https://${ms.remoteState.host}/?pair=${ms.mockId()}${ms.mockId()}`, expires_at: new Date(Date.now() + 5 * 60_000).toISOString(), ttl_secs: 300 });
    },
    confirmLinkDevice: async (code) => {
      await sleep(250);
      const waiting = ms.linkState.pending[0];
      if (!waiting || code.replace(/\D/g, "") !== "123456") throw new ApiError("no device is waiting with that code: it is wrong, expired or already used", 404);
      ms.linkState.pending = [];
      ms.linkState.devices.push({ id: `lnk_${ms.mockId()}`, label: waiting.label, paired_at: now() });
      ms.logActivity({ kind: "remote.device_approve", actor: "you", via: "cockpit", target: "remote access", section: "remote" });
      return { label: waiting.label };
    },
    revokeLinkDevice: async (id) => {
      await sleep(150);
      ms.linkState.devices = ms.linkState.devices.filter((d) => d.id !== id);
      ms.logActivity({ kind: "remote.device_revoke", actor: "you", via: "cockpit", target: "remote access", section: "remote" });
    },
    phones: () => ms.later(() => clone({ ...ms.phoneState, origins: phoneOrigins(ms) })),
    phoneInvite: async () => {
      await sleep(250);
      ms.phoneState.pending = [{ id: `ph_${ms.mockId()}`, label: "iPhone", expires_at: new Date(Date.now() + 5 * 60_000).toISOString() }];
      return clone({
        code: `${ms.mockId()}${ms.mockId()}`,
        expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
        ttl_secs: 300,
        origins: phoneOrigins(ms),
      });
    },
    confirmPhone: async (code) => {
      await sleep(250);
      const waiting = ms.phoneState.pending[0];
      if (!waiting || code.replace(/\D/g, "") !== "123456") throw new ApiError("no phone is waiting with that code: it is wrong, expired or already used", 404);
      ms.phoneState.pending = [];
      ms.phoneState.devices.push({ id: `dev_${ms.mockId()}`, label: waiting.label, paired_at: now() });
      return { label: waiting.label };
    },
    rejectPhone: async (id) => {
      await sleep(150);
      ms.phoneState.pending = ms.phoneState.pending.filter((p) => p.id !== id);
    },
    revokePhone: async (id) => {
      await sleep(150);
      ms.phoneState.devices = ms.phoneState.devices.filter((d) => d.id !== id);
    },
    tokens: () => ms.later(() => ms.apiTokens.map(clone)),
    createToken: async (body) => {
      await sleep(250);
      // The server's validation, in its order (api_tokens.rs `create`); the reason is what the 400's body says.
      const name = body?.name?.trim() ?? "";
      if (!name) throw new ApiError("name is required", 400);
      if (name.length > 120) throw new ApiError(`name is ${name.length} characters; keep it under 120`, 400);
      if (!(["read", "operate", "launch"] as string[]).includes(body?.scope ?? "")) throw new ApiError("scope must be one of: read, operate, launch", 400);
      const orgs: string[] = [];
      for (const raw of body.orgs ?? []) {
        const org = raw.trim();
        if (!org || org.includes("/")) throw new ApiError(`orgs entries must be organization names, not repositories (got "${org}")`, 400);
        orgs.push(org);
      }
      const repos: string[] = [];
      for (const raw of body.repos ?? []) {
        const repo = raw.trim();
        if (!ms.validMockRepo(repo)) throw new ApiError(`repos entries must be owner/repo (got "${repo}")`, 400);
        repos.push(repo);
      }
      if (body.max_concurrent != null && body.max_concurrent < 1) throw new ApiError("max_concurrent must be at least 1", 400);
      if (body.budget_usd_per_day != null && (!Number.isFinite(body.budget_usd_per_day) || body.budget_usd_per_day <= 0)) {
        throw new ApiError("budget_usd_per_day must be a positive number of dollars", 400);
      }
      const meta: ApiTokenMeta = {
        id: `tok_${ms.mockId()}`,
        name,
        scope: body.scope,
        orgs,
        repos,
        ...(body.max_concurrent != null ? { max_concurrent: body.max_concurrent } : {}),
        ...(body.budget_usd_per_day != null ? { budget_usd_per_day: body.budget_usd_per_day } : {}),
        created_at: now(),
      };
      ms.apiTokens.push(meta);
      // col_ plus 64 hex, the shape of util::random_token; only this answer ever holds it.
      const token = `col_${Array.from({ length: 64 }, () => "0123456789abcdef"[Math.floor(Math.random() * 16)]).join("")}`;
      return { ...clone(meta), token };
    },
    revokeToken: async (id) => {
      await sleep(200);
      const at = ms.apiTokens.findIndex((row) => row.id === id);
      if (at < 0) throw new ApiError("no such token", 404);
      ms.apiTokens.splice(at, 1);
    }
  };
}
