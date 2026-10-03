# Usage data

Usage data is an anonymous batch of counts about how the harness is being used: how many colonies run
in parallel, how they finish, where boot time goes. **It is on by default.** On the first start the
mothership says so on stderr, prints the exact batch, and names the command that switches it off —
once, and then it stays quiet. The anonymity is by construction, not by promise: buckets instead of
exact numbers, closed vocabularies instead of free text, and a test in `crates/colonizer/src/usage.rs`
holding that line.

The batch is a [Cratefield](https://github.com/Cratefield/harness) `module-telemetry` payload
(`Cratefield/harness#413`): it is built, then validated through that crate's `Batch::parse` — the same
grammar the collector on the other side parses — and only then shown or sent, so the bytes a sender
transmits are valid by construction. It is a separate question from the [live map](telemetry.md),
with its own switch and its own random id, kept in `~/.config/colonizer/usage.json` next to the map's
`telemetry.json`.

To switch it off, with or without the mothership running:

```sh
colonizer telemetry off
```

## Sending

There is **no default endpoint**. The mothership sends nothing, ever, unless its environment names a
collector:

| Variable | What it is |
| :--- | :--- |
| `COLONIZER_TELEMETRY_ENDPOINT` | The full URL of the collector's ingest route (Cratefield's `POST /v1/telemetry/events` on the host running it). Unset — the default — means nothing is sent, whatever the switch says. |

With an endpoint named, the sender runs in the mothership's background tasks: shortly after startup,
then once an hour, it checks whether a send is due — the switch on, an endpoint named, and the last
successful send (recorded in `usage-sent.json` beside the answer) older than 24 hours — and posts the
batch. One send at most per 24 hours, whichever way you count it: a mothership that runs for ten
minutes sends at most once. A batch that cannot be sent is dropped, not queued: a non-2xx answer or a
network failure is one line on stderr naming no payload, and the next attempt waits for the next
hourly check. Nothing retries, nothing accumulates anywhere, and the user is never blocked on a send.

## Why it exists

The roadmap asks questions the issue tracker can't answer. One batch answers the broad shape of them,
each as an event whose name carries the answer:

- **Do people run colonies in parallel, or one at a time?** — `colonies.parallel_now.<bucket>`
- **Does the default image work, or does everyone pin their own?** — `sandbox.preset.<label>`,
  `sandbox.image_changed.<true|false>`
- **Is autopilot trusted?** — `autopilot.enabled.<true|false>`, and how many colonies it holds:
  `autopilot.held.<bucket>`
- **Which settings do people actually touch?** — `setting.<kind>.<key>` events
- **Do colonies finish?** — `colonies.pr_opened.<bucket>`, `colonies.no_changes.<bucket>`,
  `colonies.stopped.<bucket>`, `colonies.failed.<bucket>`
- **Where does start-up time go?** — `boot.<phase>.<bucket>` events
- **Is the provider gateway used, and how many providers feed it?** — `providers.<bucket>`
- **What breaks?** — `error.<kind>.<bucket>` events

## The payload

The grammar is Cratefield's: five fields, `schema`, `install`, `client`, `modules` and `events`, and
a closed vocabulary for every string. Every count and every duration is a bucket, so no exact number
leaves the machine. The batch is at most 64 events, and the mapping stays comfortably under that
because each observation is one event.

| Cratefield field | What it carries |
| :--- | :--- |
| `schema` | `1`, Cratefield's payload schema version — the API also reports it as `payload_version`. |
| `install` | 32 lowercase hex: the usage id without its dashes. All zeros while reporting is off or held off by the environment — a batch the sender refuses to post. |
| `client` | `{kind: "server", version, platform, arch}` naming this mothership: the release triple, and `linux`/`macos`/`other` with `x86-64`/`aarch64`/`other` mapped from the same closed platform string the live map sends. A version that is not a plain release triple (a build with a pre-release tag) rounds down to `0.0.0` rather than trim it — the grammar rejects tags, it does not trim them. |
| `modules` | `["mothership"]` — the thing being reported on. Nothing else this harness composes is declared to a collector. |

Each element of `events` is `{name, outcome, error, duration, count}`. Colonizer's batch is a set of
observations, not runs, so `outcome` is always `ok`, `error` always `none`, `duration` always
`unknown`, and `count` always `1` — the labels ride in the name, which is where a reader of
`telemetry show` meets them too. The event names, and what each maps from:

| Was (pre-#628 flat field) | Event name | What it is |
| :--- | :--- | :--- |
| `colonies.parallel_now` | `colonies.parallel_now.<bucket>` | Colonies with a running microVM right now: live colonies (starting, running, idle, waiting for an answer), not counting queued ones or ones suspended while they wait for an answer, which hold no microVM. |
| `colonies.terminal.pr_opened` | `colonies.pr_opened.<bucket>` | How finished colonies ended up: one event per status — `pr_opened`, `no_changes`, `stopped`, `failed`. |
| `sandbox.preset` | `sandbox.preset.<label>` | The stack preset's id: `auto`, `node`, `python`, `rust`, `go` or `custom` — or `unknown` if a hand-edited `modules.json` names a preset the harness has never heard of. What is configured is what is sent: an install left on `auto` reports `auto`, not the stack it detected for each repository. |
| `sandbox.image_changed_from_default` | `sandbox.image_changed.<true\|false>` | Whether the image a colony actually boots differs from the one the resolved stack names. Only the comparison is sent; the image string itself is user free text and never is. |
| `autopilot.enabled` | `autopilot.enabled.<true\|false>` | Whether new colonies publish automatically: the publish module's default for this install. |
| `autopilot.held` | `autopilot.held.<bucket>` | Colonies autopilot is holding back from publishing. |
| `settings_set` | `setting.<kind>.<key>` | One event for every module setting this install carries that its schema declares — `setting.agent.model`, `setting.sandbox.preset`, and so on. Names only, never values; keys a hand-edited `modules.json` added but the schema doesn't declare are dropped. |
| `boot_ms` | `boot.<phase>.<bucket>` | One event per boot phase with samples, in boot order — `issue`, `git`, `providers`, `mesh-start`, `image-pull`, `vm-boot`, `mesh-join`, `agentd` — each the median duration across the colonies this install has booted, bucketed. Phases with no samples are left out, and so are colonies whose boot never finished: a boot still under way or one that stopped part way has no `total_ms`, and counting it would put different colonies behind each phase's median. |
| `providers` | `providers.<bucket>` | Model providers configured on the mothership. |
| `error_kinds` | `error.<kind>.<bucket>` | How failures and attention reasons are distributed, as closed labels: `agentd_not_ready`, `harness_restarted`, `vm_stopped`, `publish_interrupted`, `publish_unconfirmed` (the fixed messages the harness itself writes), `stalled`, `waiting_for_answer`, `nudges_exhausted` (the watchdog's reasons), `autopilot_held`, `model_error` (the provider gateway's model or provider failure) and `agent_failed` (the agent's runner never started). A colony's own error text names no kind, so these events can cover less than `colonies.failed` does. |

The bucket edges, exactly as the code draws them:

- **Counts:** `0` · `1` · `2-3` (2–3) · `4-7` (4–7) · `8-15` (8–15) · `16-63` (16–63) · `64+` (64 or more)
- **Durations:** `<1s` (0–999 ms) · `1-2s` (1,000–1,999) · `2-5s` (2,000–4,999) · `5-15s` (5,000–14,999) ·
  `15-60s` (15,000–59,999) · `60s+` (60,000 or more)

When there is an even number of samples, the median is the upper of the two middles. Colonizer's
duration buckets are kept even though Cratefield's grammar names coarser ones (`under-100ms` …
`over-10m`): the edges do not line up, and claiming a coarser bucket would sometimes be false, so the
honest value is `duration: "unknown"` with the real bucket in the name.

## A real batch

From a busy mothership: two colonies up, one of them waiting for an answer, autopilot holding a third
back from publishing, three pull requests opened, two colonies that changed nothing, one stopped
because its microVM died early, one failed because agentd never came up, the Go preset with a custom
image, the agent's model set, two model providers:

```json
{
  "schema": 1,
  "install": "fb50329331e245e58fa1c647393a5621",
  "client": {
    "kind": "server",
    "version": "0.1.9",
    "platform": "linux",
    "arch": "x86-64"
  },
  "modules": ["mothership"],
  "events": [
    { "name": "colonies.parallel_now.2-3", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "colonies.pr_opened.2-3", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "colonies.no_changes.2-3", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "colonies.stopped.1", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "colonies.failed.1", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "sandbox.preset.go", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "sandbox.image_changed.true", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "autopilot.enabled.true", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "autopilot.held.1", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "setting.agent.model", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "setting.sandbox.image", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "setting.sandbox.preset", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.issue.<1s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.git.<1s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.providers.<1s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.mesh-start.<1s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.image-pull.15-60s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.vm-boot.2-5s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.mesh-join.<1s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "boot.agentd.<1s", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "providers.2-3", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "error.agentd_not_ready.1", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "error.autopilot_held.1", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "error.vm_stopped.1", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 },
    { "name": "error.waiting_for_answer.1", "outcome": "ok", "error": "none", "duration": "unknown", "count": 1 }
  ]
}
```

Switched off, the same batch has the all-zero `install` — `"00000000000000000000000000000000"` — and
nothing else differs; that is the batch the sender refuses to post.

## What is never in it

- No repository, organisation, branch or issue names — nothing that identifies what you work on.
- No paths, no `pr.md`, no diffs.
- No prompts, agent output, terminal output or commit messages.
- No tokens, keys, provider base URLs, mesh addresses or IP addresses. No model names or image
  strings either — those are user free text.
- No setting *values*, only the keys that were set.
- No free-text error messages, only the closed `error.<kind>` labels above. A failure the harness did
  not name itself contributes nothing.

And the one that carries the rest: **nothing is sourced from inside a colony.** Colony output is
untrusted data ([vision.md](vision.md), principle 6) and must never become a payload we send
ourselves. A test in `crates/colonizer/src/usage.rs` holds this line: it builds a batch from sessions
full of hostile fixtures — private repositories, tokens, home paths, injection strings — and asserts
that none of it appears, and that every string that does appear is in the closed vocabulary above.

## What the first start prints

The mothership prints once, to stderr — on the first start, while nobody has answered and no
environment switch keeps it off — and then never again: a line saying anonymous usage reporting is
on, counts and bucket labels only and nothing identifying, then the exact batch it would send,
pretty-printed as JSON, then that nothing is sent unless `COLONIZER_TELEMETRY_ENDPOINT` names a
collector (and then at most one batch a day), and how to switch it off — `colonizer telemetry off`,
plus `COLONIZER_TELEMETRY`, `DO_NOT_TRACK` and `CI`, which also keep it off. The notice exists so the
default state is never a surprise. Later starts stay quiet, and so do starts after you have answered,
in Settings or on the command line. A blocked environment (`DO_NOT_TRACK`, `COLONIZER_TELEMETRY=off`,
`CI=true`) silences the notice entirely.

## How to see it

`colonizer telemetry show` prints to stdout, verbatim, the exact bytes of the last batch the mothership
built — kept in `usage-last.json` beside the answer, so the command needs neither the network nor a
running mothership. When no batch has been built yet, a fresh install or a mothership never started, it
builds the empty batch and prints that instead — so whatever shape the machine is in, "what exactly
would you send?" has a one-command answer. That empty batch carries the kept usage id as its
`install` — all zeros until a mothership has built its first batch. Anything that is not the batch
goes to stderr.

A batch is built when the mothership starts, when the sender checks for a due send, and when `GET` or
`PUT /api/telemetry/usage` is called — in practice, while the Settings pane is open. `telemetry show`
therefore prints the batch as of the last build, not a freshly computed one. On an install where the
mothership has run but nothing has polled the API since, that is the batch from the last start or the
last due send — the empty batch, on a fresh install.

Settings, under **Usage data**, shows the same batch — the one in the JSON `GET /api/telemetry/usage`
returns. The batch is shown whatever the switch says; that is the point. It is built by the same
function the sender calls, so what you read here is what goes out.

The switch itself is `PUT /api/telemetry/usage` with `{"enabled": true}` or `{"enabled": false}` —
what the pane's toggle calls. Both routes need the owner token, and the `PUT` answers with the same
body as the `GET`: `{enabled, blocked_by, payload_version, batch}`.

## Keeping it off

`colonizer telemetry off` switches it off and writes `usage.json` itself: the answer is kept, the id is
forgotten, and neither the network nor a running mothership is needed — a mothership that *is* running
re-reads the file before it consults the answer, so the change takes effect without a restart, and the
sender stops with it. `colonizer telemetry on` switches it back on, with a fresh id. The same switch is
in Settings, under **Usage data**.

Three environment switches also keep it off, whatever the stored answer says, and the UI cannot
override them: the API answers 409 while one is set. The CLI records the answer all the same and says
which variable is overriding it. Environment beats the file, so a machine nobody answers questions on
can be locked down in one place. First match wins:

| Variable | Counts as off | Notes |
| :--- | :--- | :--- |
| `COLONIZER_TELEMETRY` | `off`, `0`, `false` or `no`, case-insensitive | The app's own switch, read the same way the [live map](telemetry.md) reads it. Any other value, including `on`, does not block. |
| `DO_NOT_TRACK` | anything except empty, `0` or `false` | [consoledonottrack.com](https://consoledonottrack.com). `1`, `yes` and `true` all count. |
| `CI` | exactly `true`, case-insensitive | `CI=1` does not count. Scoped to usage data only; the live map does not read it. |

## The id is not the live map's id

The live map keeps an `install_id` in `telemetry.json`; usage data keeps a `usage_id` in
`usage.json`, which becomes the payload's `install` field with its dashes stripped. Both are random
UUIDs, and they are deliberately never the same value, in different files, so the two datasets cannot
be joined. Someone who puts a dot on the public map has not thereby linked their machine into the
usage batch, and a mothership that reports usage data has given nothing to whoever reads the map.
Each on-period gets a fresh id — switching off forgets the old one — so two periods cannot be joined
either, and Cratefield's consent rules bound an id's life anyway: it rotates after 30 days, so no id
lives longer than that even on an install that never touches the switch again.

## What shipped, and what is still open

The sender composes Cratefield's telemetry module, not a bespoke HTTP client: the module asked for in
`Cratefield/harness#413` and landed there on 2026-09-19 as `cratefield-module-telemetry`
(`crates/module-telemetry`) is a crates.io dependency of `crates/colonizer`
(`cratefield-module-telemetry = "0.2"`), and the batch is
validated with its `Batch::parse` before it is shown or sent. What remains open is elsewhere:

1. **colonizer.dev says the harness reports anonymous usage data** — the copy describing what is
   collected, and the site's `llms.txt` (there is no `llms.txt` in this repository; it is the site's).
   The website, including its footer and its `COPY.md`, where the site tracks its claims against this
   repository, lives in a different repository, so none of this can be done from this one.
2. **The site footer's "No tracking cookies" claim is qualified.** It stays true about cookies and
   becomes misleading about the product: usage data is tracking, whatever the footer says.

[vision.md](vision.md), principle 4 — "Local-first and self-contained … no cloud account required." —
is answered the way this page describes: the endpoint is opt-in by omission, the batch is anonymous —
bucketed, enumerated, drawn from a closed vocabulary — and printable on the machine that produced it,
at any moment, with `colonizer telemetry show`. And because the batch the sender transmits is exactly
what `GET /api/telemetry/usage` shows — built by the same function, validated by the same grammar —
this document keeps describing the whole thing.
