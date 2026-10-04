# Accounts, tokens and install settings

Part of the [Colonizer protocol](../protocol.md).

## Add your phone

A phone pairs in four steps and ends up with a credential of its own (`crates/colonizer/src/phone.rs`):

1. `POST /api/phone/invites` (owner, not a phone) answers `{code, expires_at, ttl_secs, origins}`:
   an invite of 256 random bits, single use, live 300 s, kept as a SHA-256 in memory (**429** with
   four open). `origins` is where a phone could reach this mothership, best first, each
   `{kind: relay|tailnet|lan, url, reachable, secure, note}`: `https://<install>.my.colonizer.dev`
   while remote access is on (reachable while connected), the tailnet address, the LAN address. A
   plain-HTTP origin is reachable only when the listener answers on it **and** the Host allowlist
   accepts it; `note` names the fix or the no-HTTPS caveat.
2. An unauthenticated `GET /?pair=<invite>` spends the invite and opens a pairing bound to that
   browser: the answer is a page showing a six-digit confirm code, with a `colonizer_pair` cookie
   (`HttpOnly`, `SameSite=Strict`, `Path=/api/phone/claim`) holding the pairing's id and secret. An
   unknown, spent or expired invite gets the locked page. No credential is set.
3. `POST /api/phone/pairings/confirm {"code"}` approves the pairing showing that code: local only
   (**403** through the remote tunnel) and never from a phone. **404** for a code no pairing shows.
   `POST /api/phone/pairings/{id}/reject` turns one down.
4. `POST /api/phone/claim`, admitted without a token, is the pairing page's poll: **202** while
   unconfirmed, **404** once expired, rejected or unknown, and **200** once confirmed, setting the
   phone's own `colonizer_token` cookie (`cph_…`, stored as a SHA-256 in `<config_dir>/phones.json`)
   and consuming the pairing.

Failed steps (an invite that opens nothing, a claim that finds nothing, a wrong confirm code)
share one limit: after 10 in 60 s every step answers **429** until the window slides.
`GET /api/phone` lists `{devices: [{id, label, paired_at}], pending: [{id, label, expires_at}]}`,
never a code or a hash. `DELETE /api/phone/devices/{id}` revokes one phone (owner, not a phone),
and with it the push subscriptions that phone made and every notification answer token sent to it
(`POST /api/push/answer` then answers **401**). A push subscription made with a phone's cookie
records that phone (`phone` in its summary) and, without a label, takes the phone's; the phone may
`PATCH`, `DELETE` or test only its own subscriptions (**403** otherwise).
Revoking a phone, or a scoped API token (`DELETE /api/tokens/{id}`), takes effect at once: every
request that credential has in flight answers **401**, a streamed body it is reading ends, and its
open WebSockets (events, terminal, `/api/stream`) close.

## Scoped API tokens

A scoped token is a named, least-privilege key for a CLI or automation, so it never holds the
per-install owner token. It authenticates as `Authorization: Bearer` only — the browser cookie
stays owner-only — and carries an ordered scope, `read` < `operate` < `launch`; optional org and
repo limits (empty lists mean no limit, both must match); and optional launch caps
(`max_concurrent`: the most of its colonies not yet terminal, where one with an open pull request no
longer counts; `budget_usd_per_day`: the most its colonies created this UTC day may spend, Claude and
routed together).

- `read` watches: `GET /api/status`, `/api/version`, `/api/sessions` (filtered to the token's
  limits), `/api/sessions/{id}`, `/api/sessions/{id}/question`, `/api/sessions/{id}/diff`,
  `/api/sessions/{id}/commits`, `/api/sessions/{id}/files` (the artifact list, single download and archive, §7.5),
  `GET /api/loops` and `/api/loops/{id}/runs` (filtered the same way), the events WebSocket, the
  `GET /api/maps/…` reads, `GET /api/merge-train`, `GET /api/merge-train/loop`, `GET /api/supply-chain-loop`, and `GET /api/tokens/self`. The terminal
  WebSocket is owner only.
- `operate` adds driving colonies that exist: `POST /api/sessions/{id}/answer|messages|stop|resume|prewarm`. Over the
  events WebSocket its commands work; a `read` token's commands are refused with a warn on the
  transcript, and no scope may switch a colony's model — that stays with the owner.
- `launch` adds starting colonies — `POST /api/sessions`, and loops of its own: `POST /api/loops`,
  `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`. A loop a token creates records the
  token; each run is admitted against the token's limits, caps and budget and marked as external
  input. Revoking the token ends the loop the next time it would run (run-now answers **409**), so
  nothing launches after revocation. A token edits and runs only the loops it created — an owner's
  loop reads as **404** — and map loops, whose runs launch outside any token's caps, stay
  owner-only.
- `fleet` sits outside that ladder and cannot be minted here: fleet pairing
  ([fleet.md](../fleet.md), below) hands it to a member at approval, and it reaches only
  `GET /api/hosts` and `POST /api/fleet/peer/leave`.

Anything else is **403** naming the token's scope and the route; a colony- or map-scoped route for
a repository outside the token's org/repo limits is **404**, the same answer an unknown id gets, so
the token can learn nothing beyond what it was granted. A launch past the concurrency cap or the
daily budget is **429** with the reason; a launch naming a repository outside the limits is
**403**. A colony launched by a token records its id in `launched_by_token` (never the secret); its
instructions are marked in the prompt as external input from the token, and its answers and
messages over the API carry a short external-input marker, so the agent reads them as a
description of the task — not the maintainer's voice. The activity log records the actor as
`token:<name>`.

## Connections: GitHub and Claude credentials

All owner only. Tokens are never sent back to the browser.

| Method & path | Purpose |
| --- | --- |
| `POST /api/settings/github-token` | `{token}` (non-empty, no whitespace): checked with `gh api user` (**400** "GitHub rejected this token" when that fails), then saved as `<config>/github-token`. A saved token wins over `GH_TOKEN`/`GITHUB_TOKEN`, and those win over the `gh` CLI login. Answers `{login}` |
| `DELETE /api/settings/github-token` | Removes it: `{ok: true}` |
| `GET /api/claude-accounts` | Named Claude credentials. A colony runs on the account it names, else its org's `agent.claude_account`, else the install default. `[{id, label, is_default, kind: "ANTHROPIC_API_KEY"\|"CLAUDE_CODE_OAUTH_TOKEN"\|null, source, added_at}]`. Names live in `<config>/claude-accounts.json`, each secret in `<config>/claude-accounts/<id>` (or the keychain). The first read moves a pre-accounts `<config>/claude-token` into an account named `default` |
| `POST /api/claude-accounts` | `{id?, label?, token}`: `token` must start with `sk-ant-` and hold no whitespace; `id` is 1–40 of `a-z 0-9 -`, derived from `label` when absent. The same `id` again replaces its token and label. The first account added becomes the default. Answers the list entry; **400** for a bad token or id |
| `DELETE /api/claude-accounts/{id}` | `{ok: true}`. **404** for an unknown id; **409** while it is the default, an org's settings name it, or a live colony runs on it (the message says which). There is no route that changes the default |
| `POST /api/settings/claude-token` · `DELETE /api/settings/claude-token` | The older single Claude token, `{token}` (`sk-ant-…`, **400** otherwise), kept at `<config>/claude-token`. Answers `{ok: true}` |
| `GET /api/claude-login` | The "Log in with your Claude subscription" flow: `{state: "idle"\|"starting"\|"awaiting_code"\|"verifying"\|"done"\|"error", url, message}`. The mothership runs `claude setup-token` in a pseudo-terminal; `url` is the sign-in link, set once `state` is `awaiting_code`. One flow per mothership, held in memory |
| `POST /api/claude-login/start` | Starts a flow (replacing any running one) and answers the state. It times out after 15 minutes. On success the token is saved to `<config>/claude-token` and `state` is `done` |
| `POST /api/claude-login/code` | `{code}` (printable ASCII, 1–2000 bytes, **400** otherwise): the code the sign-in page showed, typed into the flow. **409** when no flow is waiting for a code |
| `POST /api/claude-login/cancel` | Ends the flow: `{state: "idle", url: null, message: null}` |

## Device and install settings

| Method & path | Purpose |
| --- | --- |
| `GET /api/login-item` | Whether the mothership starts when you log in: a LaunchAgent `dev.colonizer.mothership` on macOS, the systemd user unit `colonizer.service` on Linux. `{platform: "macos"\|"linux"\|"unsupported", installed, enabled, pid, definition, binary, log, note}`; on Linux `note` suggests `loginctl enable-linger` when lingering is off. **500** on an unsupported platform |
| `POST /api/login-item` | `{enabled}`, the same as `colonizer login-item enable\|disable`. Enabling writes and loads the definition (restart on crash only, output to `<data>/mothership.out`), copying only `PATH` and non-secret `COLONIZER_*` variables into it. Disabling unloads it for future logins and never stops the running mothership. Answers the status |
| `GET /api/push/key` | `{public_key}`: the Web Push (VAPID) public key, base64url. The key pair is made on first use and kept at `<config>/push-vapid-key`; `COLONIZER_VAPID_SUBJECT` sets the JWT subject |
| `GET /api/push/subscriptions` | `[{id, label, created_at, endpoint_host, last_seen, prefs, phone}]` (`phone` the paired phone's id that subscribed it, else `null`; `created_at` and `last_seen` in unix seconds, `last_seen` `null` until the device's first presence ping). Endpoints and keys are never returned; the records are kept 0600 in `<config>/push-subscriptions.json` |
| `POST /api/push/subscriptions` | A browser's `PushSubscription.toJSON()` plus an optional `label`: `{endpoint (https, ≤ 2048), keys: {p256dh, auth}, label? (default "This device", ≤ 60)}`. The same endpoint again refreshes its keys and label. Subscribing enrolls the device: it then receives the notify module's announcements (§6.3, Notify) as one short line and a link, while that module is on, subject to the device's own preferences (below); a subscription from before preferences existed behaves on the defaults. Answers the summary; **400** on a bad body. A push service answering 404 or 410 removes the subscription. Every announcement carries `{title, body, url, tag, badge}`, plus `colony` when there is one — `tag` is `colony-<id>`, so a colony's next notification replaces its last; `badge` is the needs-you count at send time, left out for a device whose `badge` preference is off. A colony that has just been handled (`/seen` above, or its question answered) instead sends the silent `{"type":"resolved","colony","badge"}` (only while the notify module is on, and only to devices whose preferences could have announced that colony — in scope, with at least one colony event on; quiet hours and presence do not apply; `badge` again follows the device's preference), `Urgency: normal`, never to Apple endpoints (`*.push.apple.com`), which revoke a subscription that receives an invisible push |
| `PATCH /api/push/subscriptions/{id}` | `{label?, prefs?}`, either optional and `None` leaving it as it is. The label follows the create rules; `prefs` is `{events: {question\|pull_request\|needs_rebase\|failed\|attention\|provider_degraded\|digest: bool}, question_sound: bool, answer_actions: bool, badge: bool, scope: ["org"\|"org/repo", ≤ 50 entries], quiet: {start, end} \| null, questions_break_quiet: bool, tz?, utc_offset}` — a missing event key means that event's default (the act-now events on, `provider_degraded` and `digest` off), scope entries ≤ 200 characters with no whitespace and at most one `/`, quiet hours in minutes since midnight with `start ≠ end` wrapping midnight, and the offset within ±14 hours. Quiet hours follow `tz` when it names an IANA zone (daylight saving included) and `utc_offset` otherwise. `answer_actions` (answer buttons on a question) and `badge` (the app-icon count) default on. The mothership checks these before every send (`push_prefs.rs`). Answers the updated summary; **400** for bad prefs; **404** for an unknown id |
| `DELETE /api/push/subscriptions/{id}` | **204**; **404** for an unknown id |
| `POST /api/push/subscriptions/{id}/test` | One test push to that device alone, through the same encryption and prune-on-Gone as a real event but past every preference — the point is to prove the pipe works whatever the device's settings say. `{sent: bool}`; **404** for an unknown id |
| `POST /api/push/presence` | The cockpit's heartbeat: `{endpoint, colony?, focused, tz?, utc_offset?}`. Records what the tab is showing and whether it could show a notification itself, so a send to a device already looking at that colony is held back while the report is fresh (the cockpit pings every 30 s; one older than 75 s is ignored — in-memory only, so a restart errs toward buzzing). Also refreshes the device's `last_seen` and time zone (tz name and offset, for quiet hours). **204**; **404** for an unknown endpoint; **400** for an offset beyond ±14 hours |
| `POST /api/push/answer` | A question notification's button tap as an answer: `{"token", "choice": <label>}` or `{"token", "other": <text>}` → `{answered}`. The token is minted as the question push is built — a question is notification-answerable only when it is the colony's exactly-one open question, single-select, with one to three options; any other question, and every device whose `answer_actions` preference is off, gets an empty `answer` and the notification only opens the cockpit (no token is minted when no receiving device shows buttons). The token is random and single-use (first tap wins — one token per push, so a second subscribed device's tap answers **401**), the mothership keeps only its SHA-256, in memory: it works for that colony and question only, expires after 24 hours or when the question closes or is replaced, and is gone on a restart. It is never an API token, and it is this route's only credential — no cookie or bearer is read (the Host allowlist above still applies). The answer rides the same path as `POST /api/sessions/{id}/answer`, so a suspended colony holds it until restore. **401** for an unknown, spent or expired token; **409** when the colony is no longer asking that question — answered in the cockpit first (exactly one answer lands), or a new question under the same id, since the token also pins the question's content — which burns the token or cannot take an answer; **400** for a bad body or a choice the push never offered; **404** for a colony that no longer exists |
| `GET /api/archive` | The local log archive: when a colony ends, its session directory is packed to `<data>/archive/<org>/<repo>/<yyyy>/<mm>/<id>.tar.zst` (a later change adds `<id>.r2.tar.zst`, and so on), with a `.json` sidecar. `{root, count, bytes, entries: [{session, repo, org, issue, title, status, pr_url, cost_usd, model_usage, model_tier, agent, created_at, updated_at, archived_at, mothership, revision, bundle, bytes, fingerprint}]}`, newest first |
| `POST /api/archive/retention` | The only thing that deletes from the archive. `{keep_days?, max_gb?, allow_single_copy? = false, dry_run? = true, expect?}`: plans the bundles older than `keep_days`, then the oldest until the archive fits `max_gb` (1 GB = 10⁹ bytes); neither limit, nothing planned. `{dry_run, remove: [{bundle, session, bytes, archived_at}], count, bytes, kept_single_copy}`. Without `allow_single_copy: true` nothing is removed, only counted in `kept_single_copy`. Applying (`dry_run: false`) needs `expect` set to the preview's `bundle` list: **409** when the plan changed since, **400** without it or for a negative limit |
| `GET /api/hunters/{id}/probe` | A security hunter (`strix` or `shannon`): `{manifest, installed, probe: {runtime_ok, docker_ok, ready, detail}}`. Both hunters need Docker, which colonies do not have, so `ready` is always `false` today; red-team runs use colony hunters (§6.7). **404** for an unknown id |
| `POST /api/hunters/{id}/install` | Downloads the hunter binary pinned in `hunters.lock`, checks its sha256 and places it under `<data>/hunters/<id>/<version>/`. **Off by default**: **403** unless the mothership runs with `COLONIZER_HUNTER_INSTALL=1`. **400** for `shannon` (manifest only), **409** when nothing is pinned for this platform (only Linux x86_64 and aarch64 are) |
