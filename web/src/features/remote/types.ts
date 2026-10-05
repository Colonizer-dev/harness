// ---------------------------------------------------------------------------
// Web push (issue #516): the mothership pushes to phones via GET/POST/DELETE /api/push
// ---------------------------------------------------------------------------

/** One event a device can be told about; a key the prefs omit means "the default". */
export type PushEventKind =
  | "question"
  | "pull_request"
  | "needs_rebase"
  | "failed"
  | "attention"
  | "provider_quota_exhausted"
  | "provider_degraded"
  | "digest";

/** Per-device delivery prefs (issue #743), as PATCH takes and the summary answers. */
export interface PushPrefs {
  events: Partial<Record<PushEventKind, boolean>>;
  /** A sound may accompany a question's push; every other event is silent. */
  question_sound: boolean;
  /** A question's notification may offer answer buttons (issue #742). */
  answer_actions: boolean;
  /** Pushes set the installed app's badge to the needs-you count (issue #744). */
  badge: boolean;
  /** Repositories the device hears about, entries "org" or "org/repo"; empty means all. */
  scope: string[];
  /** Minutes since local midnight; start may wrap past midnight, never equals end. Null is off. */
  quiet: { start: number; end: number } | null;
  /** A question's push breaks through quiet hours when nothing else may. */
  questions_break_quiet: boolean;
  /** The device's IANA timezone, as it reported itself; null until a save that knows it. */
  tz: string | null;
  /** Minutes east of UTC (the sign of JS `getTimezoneOffset()`, negated). */
  utc_offset: number;
}

/** One enrolled device, as GET /api/push/subscriptions answers and POST returns. */
export interface PushSubscriptionSummary {
  id: string;
  label: string;
  /** Unix seconds. */
  created_at: number;
  /** The push service's host (e.g. fcm.googleapis.com); the full endpoint never reaches the list. */
  endpoint_host: string;
  /** Unix seconds of the last presence report; null until the first one. */
  last_seen: number | null;
  prefs: PushPrefs;
  /** The paired phone (issue #746) that subscribed this device, if one did; revoking it drops this subscription. */
  phone?: string | null;
}

/** POST /api/push/subscriptions: the browser's `PushSubscription.toJSON()` plus a device label. */
export interface PushSubscribeBody {
  label: string;
  endpoint: string;
  keys: { p256dh: string; auth: string };
}

/** PATCH /api/push/subscriptions/{id}: rename the device and/or replace its prefs wholesale. */
export interface PushSubscriptionPatch {
  label?: string;
  prefs?: PushPrefs;
}

/** POST /api/push/presence: where this tab is, and whether it can take the notification itself. */
export interface PushPresenceBody {
  endpoint: string;
  /** The colony this tab has open, or null when none — a push for it can be suppressed. */
  colony: string | null;
  focused: boolean;
  tz?: string;
  utc_offset?: number;
}

// ---------------------------------------------------------------------------
// Remote access (issue #535): GET/PUT /api/remote, POST /api/remote/reset
// (docs/protocol.md §6.10), plus the relay's pairing view the cockpit mirrors
// ---------------------------------------------------------------------------

/** The switch, the tunnel host and the live link, as all three /api/remote endpoints answer. */
export interface RemoteStatus {
  enabled: boolean;
  /** e.g. `h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev`; null until the first enable. The link is `https://<host>`. */
  host: string | null;
  /** True only while the switch is on and the tunnel's handshake has succeeded. */
  connected: boolean;
  /** RFC3339, only while connected: when the current tunnel came up. */
  since: string | null;
  /** True when the relay closed the tunnel because a newer one took this link over; it stays that way until a re-enable or reset dials again. */
  replaced: boolean;
}

/** One pairing code waiting at the relay (services/relay/src/worker.js `pairingView`). */
export interface RemotePairingRequest {
  /** Six digits. */
  code: string;
  github_login: string;
  /** When the code stops working, in unix seconds — the encoding the relay pins for timestamps. */
  expires_at: number;
}

/** GET /api/remote/pairing: the owner binding and the pending codes, mirrored from the relay. */
export interface RemotePairing {
  owner: { github_login: string } | null;
  pending: RemotePairingRequest[];
}

/** A browser signed in to the remote link with a link credential of its own (review finding R3). */
export interface LinkDevice {
  id: string;
  label: string;
  paired_at: string;
}

/** GET /api/remote/devices: the browsers signed in to the link, and those waiting for their code. */
export interface LinkDevices {
  devices: LinkDevice[];
  pending: { id: string; label: string; expires_at: string }[];
}

/** POST /api/remote/devices/invites: the single-use link to open on the other device. Never a credential. */
export interface LinkInvite {
  url: string;
  /** RFC3339: when the invite stops working. */
  expires_at: string;
  ttl_secs: number;
}

// ---------------------------------------------------------------------------
// Add your phone (issue #746): /api/phone — a single-use invite a phone scans,
// a code confirmed in the local cockpit, and a revocable credential per phone
// ---------------------------------------------------------------------------

/** One place the cockpit is reachable from, in the mothership's preference order (relay → tailnet → lan). */
export interface PhoneOrigin {
  kind: "relay" | "tailnet" | "lan";
  /** `scheme://host[:port]`, no trailing slash — the base the invite link is built on. */
  url: string;
  /** Whether the mothership thinks a phone can reach this origin right now. */
  reachable: boolean;
  /** False for a plain-http origin: the phone can pair, but not install the app or get notifications. */
  secure: boolean;
  /** Why the origin is (un)usable, when the mothership has something to say about it. */
  note: string | null;
}

/** POST /api/phone/invites: a single-use invite — a ticket to ask, never a credential — and where a phone might open it. */
export interface PhoneInvite {
  code: string;
  /** RFC3339: when the invite stops working. */
  expires_at: string;
  ttl_secs: number;
  origins: PhoneOrigin[];
}

/** A paired phone, with its own credential; revoking it signs that phone out alone. */
export interface PairedPhone {
  id: string;
  label: string;
  paired_at: string;
}

/** A phone that opened an invite and shows a code, waiting for it to be typed here. */
export interface PendingPhone {
  id: string;
  label: string;
  expires_at: string;
}

/** GET /api/phone. */
export interface Phones {
  devices: PairedPhone[];
  pending: PendingPhone[];
  /** The same ranked origins an invite answers with (bare origins, no code, no credential), so a
   * bookmark can name the network address without minting an invite. Older motherships omit it. */
  origins?: PhoneOrigin[];
}

// ---------------------------------------------------------------------------
// Scoped API tokens (issue #646): GET/POST /api/tokens, DELETE /api/tokens/{id}
// (docs/cli.md, "Scoped API tokens")
// ---------------------------------------------------------------------------

/** How much a token may do, ordered so `read` < `operate` < `launch` — each adds to the last.
 * `fleet` sits outside that ladder: the lowest scope there is, admitted only on the fleet routes
 * (`GET /api/hosts` and `POST /api/fleet/peer/leave`, docs/fleet.md), and minted by fleet pairing
 * rather than created by hand. */
export type ApiTokenScope = "fleet" | "read" | "operate" | "launch";

/** One token's metadata, as GET /api/tokens answers: never the secret, never its hash. */
export interface ApiTokenMeta {
  id: string;
  name: string;
  scope: ApiTokenScope;
  /** The GitHub owners the token stays inside; empty means no limit of this kind. */
  orgs: string[];
  /** The `owner/repo` repositories the token stays inside; empty means no limit of this kind. */
  repos: string[];
  /** The most colonies it may keep unfinished; absent when uncapped. */
  max_concurrent?: number;
  /** The most model spend its colonies may run up per UTC day; absent when uncapped. */
  budget_usd_per_day?: number;
  /** RFC3339. */
  created_at: string;
  /** RFC3339; absent until its first use, and refreshed at most once a minute, in memory only. */
  last_used_at?: string;
}

/** POST /api/tokens: what the cockpit's create form collects. */
export interface NewApiToken {
  name: string;
  scope: ApiTokenScope;
  orgs?: string[];
  repos?: string[];
  max_concurrent?: number;
  budget_usd_per_day?: number;
}

/** POST /api/tokens' answer: the plaintext, shown exactly once, next to the metadata. */
export interface CreatedApiToken extends ApiTokenMeta {
  token: string;
}
