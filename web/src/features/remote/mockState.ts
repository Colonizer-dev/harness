// The mock's per-call state slice for the remote feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { ApiTokenMeta, LinkDevices, Phones, PushSubscriptionSummary, RemotePairing, RemoteStatus } from "../../types";
import { ago } from "../../mockShared";
import { defaultPushPrefs } from "../../push";
import type { MockState } from "../../mockState";

export type RemoteMockState = {
    remotePairingState: RemotePairing;
    remoteState: RemoteStatus;
    phoneState: Phones;
    linkState: LinkDevices;
    remoteHost: string;
    apiTokens: ApiTokenMeta[];
    pushSubs: PushSubscriptionSummary[];
    pushLabel: (label: string) => string;
    MOCK_PUSH_KEY: string;
    remoteInstallId: () => string;
    validMockRepo: (repo: string) => boolean;
};

export function installRemoteMockState(ms: MockState): void {
  // Web push (issue #516): one device is already enrolled, so Settings has a row to show and
  // revoke, and the VAPID key has the shape of a real base64url uncompressed P-256 point. It was
  // last seen 42 minutes ago and quiet at night, so the per-device prefs editor (#743) has
  // something to show on open.
  ms.MOCK_PUSH_KEY = "BB5fVboJOnLBVPursGoy1AZA5DXhRqSdoaBnAGjI8NeR1PuBgnN3Vx6rbF5pvoxqTOhaLHQwxrRLmZgA2pHcg0k";
  // label_of (crates/colonizer/src/push.rs): trimmed, capped, "This device" when blank — the same
  // rule for a new subscription and a PATCHed one.
  ms.pushLabel = (label: string) => (label.trim() ? label.trim().slice(0, 60) : "This device");
  ms.pushSubs = [
    {
      id: "push_iphone01",
      label: "iPhone · Safari",
      created_at: Math.floor(Date.now() / 1000) - 86_400 * 2,
      endpoint_host: "fcm.googleapis.com",
      last_seen: Math.floor(Date.now() / 1000) - 60 * 42,
      prefs: { ...defaultPushPrefs(), scope: ["acme"], quiet: { start: 1320, end: 480 }, questions_break_quiet: true, tz: "Europe/Berlin", utc_offset: 120 },
    },
  ];
  // Remote access (issue #535): the switch starts off, like a fresh install's. Enabling mints the
  // host and a live tunnel; a reset changes the host, like the server's fresh identity. Install
  // ids wear the relay's shape: 20 base32 chars (services/relay/src/worker.js). One pairing code
  // waits at the relay, the shape of #534's view; confirming binds it, single-use, rejecting drops it,
  // and unbinding (or a reset) clears the owner (#599).
  ms.remoteInstallId = () => Array.from({ length: 20 }, () => "abcdefghijklmnopqrstuvwxyz234567"[Math.floor(Math.random() * 32)]).join("");
  ms.remoteState = { enabled: false, host: null, connected: false, since: null, replaced: false, require_github: false };
  ms.phoneState = { devices: [{ id: "dev_demo01", label: "iPhone", paired_at: new Date(Date.now() - 3 * 86_400_000).toISOString() }], pending: [] };
  ms.linkState = { devices: [], pending: [] };
  ms.remoteHost = "h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev";
  ms.remotePairingState = {
    owner: null,
    pending: [{ code: "481516", github_login: "octocat", expires_at: Math.floor(Date.now() / 1000) + 600 }],
  };
  // Scoped API tokens (issue #646): three rows with the shapes the list must tell apart — a capped
  // launcher in daily use, a reader that has never been used, and an operate token limited to one
  // org and repo. The plaintext of none of them is known, like the server's: only the hash is kept.
  ms.apiTokens = [
    { id: "tok_nightly7", name: "nightly burn-down", scope: "launch", orgs: [], repos: [], max_concurrent: 2, budget_usd_per_day: 5, created_at: ago(12 * 1440), last_used_at: ago(19) },
    { id: "tok_wallboard", name: "wallboard", scope: "read", orgs: [], repos: [], created_at: ago(34 * 1440) },
    { id: "tok_phoneops", name: "phone ops", scope: "operate", orgs: ["acme"], repos: ["acme/webshop"], created_at: ago(5 * 1440), last_used_at: ago(185) },
  ];
  // util::valid_repo: exactly owner/name, each 1–100 chars of [-._a-zA-Z0-9] and not "." or "..".
  ms.validMockRepo = (repo: string) => {
    const [owner, name, extra] = repo.split("/");
    return extra === undefined && [owner, name].every((p) => !!p && p.length <= 100 && p !== "." && p !== ".." && /^[._a-zA-Z0-9-]+$/.test(p));
  };
}
