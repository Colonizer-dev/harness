# Hosted: the same mothership, somewhere else

The design gate for [#298](https://github.com/Colonizer-dev/harness/issues/298). This page is a
contract, not a feature: it fixes what "hosted" means before the thing is built, so that a hosted
Colonizer inherits the local one's behaviour instead of growing a second API to match it.

**Status.** The contract is the deliverable, and two of its pieces are in the code already:
deployment-keyed credential storage (`COLONIZER_DEPLOYMENT`) and the pre-upload manifest
(`GET /api/upload/manifest`). The fleet view over several motherships has shipped since
([fleet.md](fleet.md)). The hosted service itself, the outpost agent and multi-user sign-in are
`PLANNED`: no code, no account system, no endpoint that talks to one.
Where a section says "follow-up", that sentence is the whole of what exists. Terms as in
[outposts.md](outposts.md); "hosted" is a mothership someone else runs for you.

## One API, two deployments

`COLONIZER_DEPLOYMENT` selects `local` (the default) or `hosted`; any other value counts as
hosted. The API does not change with it:

- **The routes are the same routes.** `crates/colonizer/routes.snap` is the route table, with
  method, unauthenticated answer and token requirement per path. Both deployments serve it.
- **The events are the same events.** One vocabulary on all three hops — runner, agentd,
  mothership, browser ([docs/protocol.md](protocol.md)); [agent-events.schema.json](agent-events.schema.json)
  describes the browser-facing half.
- **The errors are the same errors.** Every handler failure is an `AppError`: the HTTP status plus
  a body of exactly `{"error": "<message>"}` (`crates/colonizer/src/app.rs`), and the web client
  already reads that field and nothing else.

The browser client is the proof the seam is real. `web/src/api.ts` calls `fetch` on relative
`/api/...` paths and builds its WebSocket URLs from `location.host` (`wsUrl`); it sends no
credential of its own — the `colonizer_token` cookie rides along because the cockpit is
same-origin. Pointing that client at a hosted mothership takes exactly two changes, both in
`web/src/api.ts`: a base URL in `request()` and `wsUrl()`, and an `Authorization: Bearer` header
per request, because a cookie set by the hosted site does not follow the browser to the API.
Neither change exists yet; until it does, the web client is local-only.

**Deliberate differences, at v1: none.** The list is empty on purpose: a hosted deployment
answers what a laptop answers, status codes included. What differs is not an API difference —
deployment mode changes what the mothership does with credentials on its own disk, not what it
says over the wire. The next sections are that list.

## Identity and access

The local posture is network-shaped: the server binds `127.0.0.1:7878` (`COLONIZER_BIND`), and
`host_guard` (`crates/colonizer/src/server.rs`) checks the `Host` header against the bind address,
`localhost` and `COLONIZER_ALLOWED_HOSTS` (DNS rebinding), and requires a same-origin `Origin` on
cookie-authenticated writes and upgrades; sign-in is the printed link (`colonizer open`), which
sets the cookie. None of that means anything on a public host: a hosted deployment is reachable
from anywhere by construction, so the network stops being a credential, and hosted carries none
of this over.

What hosted uses instead are the seams that already exist for non-browser callers:

| Seam | Today | Hosted |
| :--- | :--- | :--- |
| Per-install token | `<config_dir>/api-token`, sent as `Authorization: Bearer` by the CLI, or the `colonizer_token` cookie in the browser | The workspace credential is presented the same way: `Authorization: Bearer`, never a cookie |
| Scoped API tokens | `col_…` tokens in `<config_dir>/api-tokens.json` (`api_tokens.rs`): named, scope `read` < `operate` < `launch`, optional org and repo limits, SHA-256-hashed, plaintext shown once, managed in the UI | The workspace credential is this kind of token: scoped to one workspace, created, rotated and revoked in the UI |

The contract for that credential, in terms the local code already enforces:

- It travels as a bearer header, never appears in browser code, never lands in git.
- Its scope decides the routes: outside the scope is a 403 that names it; a colony outside the
  workspace is a 404 — the same answer an unknown id gets, so the token learns nothing from a
  denial.
- Rotation revokes the old value immediately: the registry file is rewritten on every change and
  read per request, never cached across one.
- Changing the account's password signs out the other browser sessions. The local analog is
  exact: the cookie holds the token value itself, so replacing the install token invalidates
  every signed-in browser at once.

Multi-user auth — accounts, invitations, who may see whose colonies — is a separate design and a
follow-up; this page deliberately does not sketch it.

## Credentials at rest

With `COLONIZER_MASTER_KEY` set, every credential the mothership saves is envelope-encrypted —
ChaCha20-Poly1305 via `ring`, the key being the SHA-256 of the variable's value — and stored as
`<path>.enc` beside where the plaintext would sit, which is removed. Reads fail closed: a `.enc`
file with content opens under the key or the credential counts as missing — never a stale
plaintext fallback, never ciphertext handed back as if it were the key. With the variable unset,
local mode writes the plaintext file 0600, and only that; the fallback is documented and
intentional, because a laptop install should not require a key.

Under `COLONIZER_DEPLOYMENT=hosted` the fallback is gone: saving any credential without
`COLONIZER_MASTER_KEY` is refused — nothing is written, and the existing value is left alone. The
check sits under every path that writes a credential file: the secret writer and its file half
alike, so moving a secret out of the system keychain onto its file is refused the same way. The
provider-key save (`PUT /api/providers/{id}`) answers the refusal with a 503 whose message names
both variables. The rule covers everything that goes through the secret writer: provider keys, the
GitHub token, the Claude credential, colony secrets, the notification signing secret, the memory,
voice and push keys. This is an extra mothership-side layer only; colony-facing mechanics are
unchanged, and are the ones the [trust model](trust-model.md) names: per-colony gateway
tokens, placeholders swapped for the real credential at the TLS edge, agentd's per-session bearer
token, 0600 files.

Two limits worth stating, because they are true locally too: a secret the system keychain holds
has no `.enc` file, and the install token (`api-token`) is plaintext on purpose so the CLI can
read it off disk. Neither is affected by the key.

### Recovery

Changing or losing `COLONIZER_MASTER_KEY` makes every stored `.enc` undecryptable. The mothership
keeps running, but each credential reads as missing, so GitHub calls, provider calls and anything
else that needs one fail until it is entered again. Rotation from an old key to a new one is the
same operation as recovery — re-entering — because there is no re-key command (follow-up):

1. Set the new `COLONIZER_MASTER_KEY` and restart the mothership.
2. Re-enter each credential in Settings. Every save writes a fresh `.enc` under the new key and
   removes the stale one, so no separate cleanup is needed: the provider keys (`provider-keys/`),
   the GitHub token (`github-token`), the Claude credential (`claude-accounts/<account>` — the
   token routes save into the default account, and a pre-accounts `claude-token` is migrated into
   `claude-accounts/default`), colony secrets (`colony-secrets/<ENV>`),
   the notification signing secret (`notify-secret`), the mem0 key (`memory-keys/mem0`), the
   voice keys (`voice-keys/<provider>`) and the web-push VAPID key (`push-vapid-key`).
3. Alternatively, delete the stale `.enc` files under the config directory first and then
   re-enter. Both orders end in the same state; deleting first keeps the dead ciphertext out of
   backups sooner.

## Config-only upload

A hosted mothership needs configuration, not your machine's state. `GET /api/upload/manifest`
returns `{copied, stays_home, digest}` — what an upload would carry, before anything is uploaded.
Each entry is `{name, kind, reason}`: the config-dir-relative name, `config` or `plugin` on the
copied side and `file` or `dir` on the stays-home side, and why — credentials, telemetry, usage,
host state, hand-edited, or `unknown` for anything not recognised.
`copied` is an allowlist, and the whole of it is this:

| Copied | What it is |
| :--- | :--- |
| `modules.json` | Which modules are selected, and their settings |
| `providers.json` | The providers and their model maps — no keys; keys live in `provider-keys/` and stay home |
| `orgs.json` | Workspace overrides: budgets, quotas, stacks, egress, memory, watchdog, notifications, sensitivity provider marks |
| Skill packs | By name and version — the pin, never their files |

Everything else stays home. The rest of the config directory — every credential, the live-map and
usage answers (`telemetry.json`, `usage.json`), `host_id`, the relay settings, the scoped-token
registry, `colonizer.toml`, `known-orgs.json`, anything the mothership does not recognise — and
the whole data directory: colonies, sessions, worktrees, clones, mesh state, generated files.
None of it is uploaded, by allowlist rather than by effort.

The `digest` is what a future confirm step echoes back: the user is shown this manifest and
confirms this digest, so the upload is exactly what was seen, not a manifest fetched fresh
afterwards. Re-uploading replaces the hosted copy wholesale — no merge, so a stale workspace
setting cannot survive beside a fresh one. The transfer and the confirmation dialog are
follow-ups; there is no hosted service to receive anything yet.

## The outpost seam

[docs/outposts.md](outposts.md) draws the control/execution seam this rides on; read it rather
than a duplicate here. What hosted adds to it, as contract:

- **Auth.** An outpost presents its own credential, scoped to the outpost role — the same shape
  as agentd's per-session token and the scoped API tokens above: random, hashed on the mothership,
  revocable by deletion — and joins the mesh with a single-use join key, the way a colony does.
  Enrollment is an explicit act on both ends, never discovery.
- **Colony spec.** What the mothership sends an outpost is what a local colony gets: the
  worktree, placeholders instead of credentials, and a per-colony gateway token. No GitHub token,
  no provider key, no signing key crosses the seam.
- **Events.** Colonies on an outpost report in the same vocabulary, back to the mothership,
  unchanged from [protocol.md](protocol.md). Nothing new on the wire.

All of it is a follow-up; outposts.md's `PLANNED` verdict on protocol and enrollment stands.

## The audit gate

The v0.1.3 audit's verdict stands, and hosted does not soften it: [docs/audit.md](audit.md) has
the findings and the gates. Hosted and outpost handling of sensitive repositories stays blocked
until the four draft advisories (F01–F04) are fixed and re-verified — gate G1, negative tests on
real colonies. Until then, only public, low-sensitivity repositories may target a hosted or
outpost deployment, and that is the operator's call to make and to record per org. No `orgs.json`
field records it today — `OrgSettings` (`crates/colonizer/src/orgs.rs`) carries budgets, quotas,
stack, egress, memory, watchdog and notifications, nothing about deployment targets — so the
follow-up adds a field there, where the other per-org decisions live.

## Follow-ups

Each of these is unbuilt; naming them keeps them out of the contract above: the hosted service
itself, with the upload transfer and its confirmation dialog; the workspace credential, issued
per workspace, bearer-only, managed in the UI; a re-key command, so key rotation stops meaning
re-entering every secret; the outpost agent — enrollment, the colony-spec wire format, placement
on real nodes; multi-user auth — accounts and who may
see whose colonies; and the per-org record of the audit gate's deployment decision.
