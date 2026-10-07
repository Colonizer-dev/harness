// Remote access, phones, push & API tokens API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request } from "../../http";
import type { ApiTokenMeta, CreatedApiToken, LinkDevices, LinkInvite, NewApiToken, PhoneInvite, Phones, PushPresenceBody, PushSubscribeBody, PushSubscriptionPatch, PushSubscriptionSummary, RemotePairing, RemoteStatus } from "./types";

export interface RemoteApi {
  /** GET /api/push/key: the VAPID public key the browser subscribes with (issue #516). */
  pushKey(): Promise<{ public_key: string }>;
  /** GET /api/push/subscriptions: every device the mothership pushes to. */
  pushSubscriptions(): Promise<PushSubscriptionSummary[]>;
  /** POST /api/push/subscriptions: enrols this browser's subscription under a label; 400 on invalid input. */
  subscribePush(body: PushSubscribeBody): Promise<PushSubscriptionSummary>;
  /** DELETE /api/push/subscriptions/{id}: revokes one device. */
  deletePushSubscription(id: string): Promise<void>;
  /** PATCH /api/push/subscriptions/{id}: renames a device and/or replaces its prefs; 400 on bad prefs, 404 unknown. */
  updatePushSubscription(id: string, body: PushSubscriptionPatch): Promise<PushSubscriptionSummary>;
  /** POST /api/push/subscriptions/{id}/test: one push the device should actually show. */
  testPushSubscription(id: string): Promise<{ sent: boolean }>;
  /** POST /api/push/presence: the focused-tab report; 404 once the endpoint is no longer subscribed. */
  pushPresence(body: PushPresenceBody): Promise<void>;
  /** GET /api/remote: the remote-access switch, the tunnel host and the live link (issue #535, docs/protocol.md §6.10). */
  remote(): Promise<RemoteStatus>;
  /** PUT /api/remote: switches the tunnel on or off. 502 when the relay refused the registration — the switch stays off; 500 when the key file is broken and needs a reset. */
  setRemote(enabled: boolean): Promise<RemoteStatus>;
  /** POST /api/remote/reset: a fresh key and host; the old link stops working. */
  resetRemote(): Promise<RemoteStatus>;
  /** PUT /api/remote/require-github: whether the relay asks for GitHub sign-in before the pair code (#1086). Local-only (403 through the link); 502 when the relay refused or predates the setting — nothing changes then. */
  setRemoteRequireGithub(requireGithub: boolean): Promise<RemoteStatus>;
  /** GET /api/remote/pairing: the relay's owner binding and pending codes, fetched with the install's signed call (issue #599). 409 while remote access has never been on (no link), 502 when the relay is unreachable or no longer knows this install. */
  remotePairing(): Promise<RemotePairing>;
  /** POST /api/remote/pairing/confirm: binds that code's GitHub account as the owner. 400 bad code, 404 unknown/expired/used, 409 owner already bound; 403 through the remote link — confirming is local-only. */
  confirmRemotePairing(code: string): Promise<{ owner: { github_login: string } }>;
  /** POST /api/remote/pairing/reject: drops one pending code, so that sign-in never becomes the owner. 404 when it is not pending; local-only like confirm. */
  rejectRemotePairing(code: string): Promise<{ github_login: string }>;
  /** DELETE /api/remote/owner: unbinds the owner and clears pending codes; the owner's relay sessions stop working. Local-only. */
  unbindRemoteOwner(): Promise<void>;
  /** GET /api/remote/devices: the browsers signed in to the remote link (review finding R3) and those waiting for their code. */
  linkDevices(): Promise<LinkDevices>;
  /** POST /api/remote/devices/invites: a single-use, five-minute link to open on the other device; it opens only through the relay. 409 while remote access is off. */
  linkInvite(): Promise<LinkInvite>;
  /** POST /api/remote/devices/confirm: approve the browser showing this code. Local-only; 404 for a wrong, expired or used code. */
  confirmLinkDevice(code: string): Promise<{ label: string }>;
  /** DELETE /api/remote/devices/{id}: signs that one browser out of the link, at once. */
  revokeLinkDevice(id: string): Promise<void>;
  /** GET /api/phone (issue #746): the paired phones and the ones waiting for their code to be confirmed. */
  phones(): Promise<Phones>;
  /** POST /api/phone/invites: a single-use, five-minute invite for a phone to scan, and the origins it might open it on. Never a credential. */
  phoneInvite(): Promise<PhoneInvite>;
  /** POST /api/phone/pairings/confirm: approve the phone showing this code. Local-only; 404 for a wrong, expired or used code. */
  confirmPhone(code: string): Promise<{ label: string }>;
  /** POST /api/phone/pairings/{id}/reject: that phone is never approved. Local-only. */
  rejectPhone(id: string): Promise<void>;
  /** DELETE /api/phone/devices/{id}: signs that one phone out. */
  revokePhone(id: string): Promise<void>;
  /** GET /api/tokens: every scoped API token's metadata, oldest first (docs/cli.md, "Scoped API tokens"). */
  tokens(): Promise<ApiTokenMeta[]>;
  /** POST /api/tokens: mints one. The plaintext in the answer is shown once and never again; 400 with the reason on bad input. */
  createToken(body: NewApiToken): Promise<CreatedApiToken>;
  /** DELETE /api/tokens/{id}: revokes at once; 404 when no token carries the id. */
  revokeToken(id: string): Promise<void>;
}

export const remoteHttp: RemoteApi = {
  pushKey: () => request("/api/push/key"),
  pushSubscriptions: () => request("/api/push/subscriptions"),
  subscribePush: (body) => post("/api/push/subscriptions", body),
  deletePushSubscription: (id) => del(`/api/push/subscriptions/${enc(id)}`),
  updatePushSubscription: (id, body) => request(`/api/push/subscriptions/${enc(id)}`, { method: "PATCH", body: JSON.stringify(body) }),
  testPushSubscription: (id) => post(`/api/push/subscriptions/${enc(id)}/test`),
  pushPresence: (body) => post("/api/push/presence", body),
  remote: () => request("/api/remote"),
  setRemote: (enabled) => put("/api/remote", { enabled }),
  resetRemote: () => post("/api/remote/reset"),
  setRemoteRequireGithub: (requireGithub) => put("/api/remote/require-github", { require_github: requireGithub }),
  remotePairing: () => request("/api/remote/pairing"),
  confirmRemotePairing: (code) => post("/api/remote/pairing/confirm", { code }),
  rejectRemotePairing: (code) => post("/api/remote/pairing/reject", { code }),
  unbindRemoteOwner: () => del("/api/remote/owner"),
  linkDevices: () => request("/api/remote/devices"),
  linkInvite: () => post("/api/remote/devices/invites"),
  confirmLinkDevice: (code) => post("/api/remote/devices/confirm", { code }),
  revokeLinkDevice: (id) => del(`/api/remote/devices/${enc(id)}`),
  phones: () => request("/api/phone"),
  phoneInvite: () => post("/api/phone/invites"),
  confirmPhone: (code) => post("/api/phone/pairings/confirm", { code }),
  rejectPhone: (id) => post(`/api/phone/pairings/${enc(id)}/reject`),
  revokePhone: (id) => del(`/api/phone/devices/${enc(id)}`),
  tokens: () => request("/api/tokens"),
  createToken: (body) => post("/api/tokens", body),
  revokeToken: (id) => del(`/api/tokens/${enc(id)}`),
};
