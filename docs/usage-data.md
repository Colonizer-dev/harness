# Usage data

Usage data is an anonymous batch of counts about how the harness is being used: how many colonies run
in parallel, how they finish, where boot time goes. **It is on by default.** On the first start the
mothership says so on stderr, prints the exact batch, and names the command that switches it off —
once, and then it stays quiet. The anonymity is by construction, not by promise: buckets instead of
exact numbers, closed vocabularies instead of free text, and a test in `crates/colonizer/src/usage.rs`
holding that line.

In this release nothing is sent at all. There is no sender in this build — no endpoint, no background
loop, nothing leaves the machine — so the batch is only built, shown and kept locally. It is a separate
question from the [live map](telemetry.md), with its own switch and its own random id, kept in
`~/.config/colonizer/usage.json` next to the map's `telemetry.json`.

To switch it off, with or without the mothership running:

```sh
colonizer telemetry off
```

## Why it exists

The roadmap asks questions the issue tracker can't answer. One batch answers the broad shape of them:

- **Do people run colonies in parallel, or one at a time?** — `colonies.parallel_now`
- **Does the default image work, or does everyone pin their own?** — `sandbox`
- **Is autopilot trusted?** — `autopilot.enabled`, and how many colonies it holds
- **Which settings do people actually touch?** — `settings_set`
- **Do colonies finish?** — `colonies.terminal`
- **Where does start-up time go?** — `boot_ms`
- **Is the provider gateway used, and how many providers feed it?** — `providers`
- **What breaks?** — `error_kinds`

## The payload

The batch is versioned: `payload_version` is `1`, bumped when the batch's shape or vocabulary changes,
so a future sender can tell periods apart. Every count and every duration is a bucket, so no exact
number leaves the machine; every other string but three comes from a closed, compile-time vocabulary.
(The three are `usage_id`, `harness_version` and `platform` — a random UUID and two fixed strings.)

| Field | Type | What it is |
| :--- | :--- | :--- |
| `payload_version` | number | `1`. |
| `usage_id` | UUID or `null` | A random UUID for the current on-period: minted when the first batch is built — reporting is on by default, so usually the first start — or when the switch is turned on, and forgotten when reporting is switched off, so the next period cannot be joined to this one. `null` while reporting is off — after `colonizer telemetry off`, or while an environment switch holds it off. Never the live map's `install_id`. |
| `harness_version` | string | The Colonizer version, e.g. `0.1.3`. |
| `platform` | string | `linux-x86_64`, `darwin-arm64` or `other` — the same closed set the live map sends. |
| `colonies.parallel_now` | count bucket | Colonies with a running microVM right now. Queued ones hold none, so they are not counted. |
| `colonies.terminal` | 4 count buckets | How finished colonies ended up: `pr_opened`, `no_changes`, `stopped`, `failed`. |
| `sandbox.preset` | closed label | The stack preset's id: `node`, `python`, `rust`, `go` or `custom` — or `unknown` if a hand-edited `modules.json` names a preset the harness has never heard of. |
| `sandbox.image_changed_from_default` | boolean | Whether the image a colony actually boots differs from the one the resolved stack names. Only the comparison is sent; the image string itself is user free text and never is. |
| `autopilot.enabled` | boolean | Whether new colonies publish automatically: the publish module's default for this install. |
| `autopilot.held` | count bucket | Colonies autopilot is holding back from publishing. |
| `settings_set` | sorted strings | `<kind>.<key>` for every module setting this install carries that its schema declares — `agent.model`, `sandbox.preset`, and so on. Names only, never values; keys a hand-edited `modules.json` added but the schema doesn't declare are dropped. |
| `boot_ms` | list | Where boot time went: one entry per boot phase with samples, in boot order — `issue`, `git`, `providers`, `mesh-start`, `image-pull`, `vm-boot`, `mesh-join`, `agentd` — each the median duration across the colonies this install has booted, bucketed. Phases with no samples are left out. |
| `providers` | count bucket | Model providers configured on the mothership. |
| `error_kinds` | map of count buckets | How failures and attention reasons are distributed, as closed labels: `agentd_not_ready`, `harness_restarted`, `vm_stopped`, `publish_interrupted` (the fixed messages the harness itself writes), `stalled`, `waiting_for_answer`, `nudges_exhausted` (the watchdog's reasons) and `autopilot_held`. A colony's own error text names no kind, so this map can sum to less than `colonies.terminal.failed`. |

The bucket edges, exactly as the code draws them:

- **Counts:** `0` · `1` · `2-3` (2–3) · `4-7` (4–7) · `8-15` (8–15) · `16-63` (16–63) · `64+` (64 or more)
- **Durations:** `<1s` (0–999 ms) · `1-2s` (1,000–1,999) · `2-5s` (2,000–4,999) · `5-15s` (5,000–14,999) ·
  `15-60s` (15,000–59,999) · `60s+` (60,000 or more)

When there is an even number of samples, the median is the upper of the two middles.

## A real batch

From a busy mothership: two colonies up, one of them waiting for an answer, autopilot holding a third
back from publishing, three pull requests opened, two colonies that changed nothing, one stopped
because its microVM died early, one failed because agentd never came up, the Go preset with a custom
image, the agent's model set, two model providers. Switched off, the same batch has `usage_id: null`
and nothing else differs:

```json
{
  "payload_version": 1,
  "usage_id": "fb503293-31e2-45e5-8fa1-c647393a5621",
  "harness_version": "0.1.3",
  "platform": "linux-x86_64",
  "colonies": {
    "parallel_now": "2-3",
    "terminal": {
      "pr_opened": "2-3",
      "no_changes": "2-3",
      "stopped": "1",
      "failed": "1"
    }
  },
  "sandbox": {
    "preset": "go",
    "image_changed_from_default": true
  },
  "autopilot": {
    "enabled": true,
    "held": "1"
  },
  "settings_set": [
    "agent.model",
    "sandbox.image",
    "sandbox.preset"
  ],
  "boot_ms": [
    {
      "phase": "issue",
      "bucket": "<1s"
    },
    {
      "phase": "git",
      "bucket": "<1s"
    },
    {
      "phase": "providers",
      "bucket": "<1s"
    },
    {
      "phase": "mesh-start",
      "bucket": "<1s"
    },
    {
      "phase": "image-pull",
      "bucket": "15-60s"
    },
    {
      "phase": "vm-boot",
      "bucket": "2-5s"
    },
    {
      "phase": "mesh-join",
      "bucket": "<1s"
    },
    {
      "phase": "agentd",
      "bucket": "<1s"
    }
  ],
  "providers": "2-3",
  "error_kinds": {
    "agentd_not_ready": "1",
    "autopilot_held": "1",
    "vm_stopped": "1",
    "waiting_for_answer": "1"
  }
}
```

## What is never in it

- No repository, organisation, branch or issue names — nothing that identifies what you work on.
- No paths, no `pr.md`, no diffs.
- No prompts, agent output, terminal output or commit messages.
- No tokens, keys, provider base URLs, mesh addresses or IP addresses. No model names or image
  strings either — those are user free text.
- No setting *values*, only the keys that were set.
- No free-text error messages, only the closed `error_kinds` labels above. A failure the harness did
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
pretty-printed as JSON, then how to switch it off — `colonizer telemetry off`, plus
`COLONIZER_TELEMETRY`, `DO_NOT_TRACK` and `CI`, which also keep it off. Nothing is sent either way;
the notice exists so the default state is never a surprise. Later starts stay quiet, and so do starts
after you have answered, in Settings or on the command line. A blocked environment (`DO_NOT_TRACK`,
`COLONIZER_TELEMETRY=off`, `CI=true`) silences the notice entirely.

## How to see it

`colonizer telemetry show` prints to stdout, verbatim, the exact bytes of the last batch the mothership
built — kept in `usage-last.json` beside the answer, so the command needs neither the network nor a
running mothership. When no batch has been built yet, a fresh install or a mothership never started, it
builds the empty batch and prints that instead — so whatever shape the machine is in, "what exactly
would you send?" has a one-command answer. Anything that is not the batch goes to stderr.

There is deliberately no background loop: a batch is built when the mothership starts, and when `GET`
or `PUT /api/telemetry/usage` is called — in practice, while the Settings pane is open. `telemetry
show` therefore prints the batch as of the last build, not a freshly computed one. On an install
where the mothership has run but nothing has polled the API since, that is the batch from the last
start — the empty batch, on a fresh install.

Settings, under **Usage data**, shows the same batch — the one in the JSON `GET /api/telemetry/usage`
returns. The batch is shown whatever the switch says; that is the point. It is built by the same
function a sender would call, so what you read here is what would go out.

The switch itself is `PUT /api/telemetry/usage` with `{"enabled": true}` or `{"enabled": false}` —
what the pane's toggle calls.

## Keeping it off

`colonizer telemetry off` switches it off and writes `usage.json` itself: the answer is kept, the id is
forgotten, and neither the network nor a running mothership is needed — a mothership that *is* running
re-reads the file before it consults the answer, so the change takes effect without a restart.
`colonizer telemetry on` switches it back on, with a fresh id. The same switch is in Settings, under
**Usage data**.

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
`usage.json`. Both are random UUIDs, and they are deliberately never the same value, in different
files, so the two datasets cannot be joined. Someone who puts a dot on the public map has not thereby
linked their machine into the usage batch, and a mothership that reports usage data has given nothing
to whoever reads the map. Each on-period gets a fresh id — switching off forgets the old one — so two
periods cannot be joined either.

## Before anything ships

Nothing is sent today: this change builds the batch, shows it and keeps the switch, locally, and sends
nothing anywhere. Adding a sender is a release, and it ships with these preconditions met:

1. **The sender composes `Cratefield/harness#413`'s telemetry module, not a bespoke HTTP client.**
   That issue is an open feature request, filed 2026-09-16: no `module-telemetry` crate exists on
   crates.io yet, and the client contract Colonizer would consume is itself unbuilt. Until it exists
   there is nothing to compose — which is why this release ships everything except egress.
2. **colonizer.dev says the harness reports anonymous usage data** — the copy describing what is
   collected, and the site's `llms.txt` (there is no `llms.txt` in this repository; it is the site's).
   The website, including its footer and its `COPY.md`, where the site tracks its claims against this
   repository, lives in a different repository, so none of this can be done from this one.
3. **The site footer's "No tracking cookies" claim is qualified.** It stays true about cookies and
   becomes misleading about the product: once a sender exists, usage data is tracking, whatever the
   footer says.

A sender also has to answer [vision.md](vision.md), principle 4 — "Local-first and self-contained … no
cloud account required." The answer is what this page already describes: nothing is sent today, and
when something is, it is anonymous — bucketed, enumerated, drawn from a closed vocabulary — and
printable on the machine that produced it, at any moment, with `colonizer telemetry show`.

When a sender does land, the batch it transmits is exactly what `GET /api/telemetry/usage` shows —
built by the same function — so this document keeps describing the whole thing.
