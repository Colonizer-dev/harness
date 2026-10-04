# Model routing, tiers and the second opinion

Part of the [Colonizer protocol](../protocol.md).

## 6.1 Model routing (runner)

Model strings are `<provider>/<model-id>` for non-Anthropic providers (e.g. `deepseek/deepseek-flash`,
`local/deepseek-flash`). A model the colony will run (after tier routing) whose `<provider>/` prefix
names no configured provider refuses the boot — a typo'd route would otherwise quietly spend Anthropic —
while anything without a provider prefix (`opus`, `claude-opus-5`, …) goes to Anthropic unchanged.

Runner environment set by the mothership:

| Variable | Meaning |
| --- | --- |
| `COLONIZER_MODEL` | Orchestrator (main thread) model; nearly all of a colony's model traffic. With per-task routing on (§6.1b) the mothership may substitute the tier's model here, chosen from the agent module's tier settings — those settings exist only on the mothership, and their variables are stripped from this environment once the tier is chosen, so only the provider actually in use is probed at boot |
| `COLONIZER_SUBAGENT_MODEL` | Model for subagents; only used when the agent delegates to one, which colonies rarely do (maps to `CLAUDE_CODE_SUBAGENT_MODEL`, with `CLAUDE_CODE_SUBAGENT_MODEL_FORCE=1` so agents that name their own model (Claude Code's built-in Explore is `inherit`) use it too) |
| `COLONIZER_IMAGE` | The container image the colony booted; the runner tells the agent what it can and cannot run |
| `COLONIZER_BACKGROUND_MODEL` | Model for small auxiliary background work (maps to `ANTHROPIC_DEFAULT_HAIKU_MODEL`) |
| `COLONIZER_MODEL_ROUTES` | JSON array of routes (below); empty or absent means Anthropic only |
| `COLONIZER_SCAN` | `off` (default), `warn` or `block`. Pre-flight scan of the workspace before the agent starts |
| `COLONIZER_SCAN_COMMAND` | The scanner to run, resolved **inside the colony**. Split on whitespace and run without a shell. Empty means no scan runs |
| `COLONIZER_PLUGIN_DIRS` | Comma-separated **in-VM** plugin directories. The mothership resolves the configured names under its own plugins folder, mounts each read-only, and rewrites this to the guest paths; the runner turns them into the SDK's `plugins: [{type:'local', path}]`. Empty or absent loads none |
| `COLONIZER_EFFORT`, `COLONIZER_SUBAGENT_EFFORT` | Reasoning effort for the orchestrator and for the general-purpose and Explore subagents (`low`, `medium`, `high`, `xhigh`, `max`); empty uses the model's default, and for subagents the orchestrator's |
| `COLONIZER_TASK_LABELS` | The issue's labels, comma-separated, so conditional instructions can depend on them |
| `COLONIZER_ENFORCE_CHOICES` | On by default: a turn that ends with a plain-text question is asked once to re-ask it with choices |
| `COLONIZER_SUMMARY_MODEL` | The `summary_model` setting (`Session.summary`, §4) |

```json
[{"provider": "deepseek", "prefix": "deepseek/", "base_url": "https://api.deepseek.com/anthropic",
  "auth": "x-api-key", "key_env": "COLONIZER_PROVIDER_KEY_DEEPSEEK"}]
```

`auth` is `x-api-key`, `bearer` or `none`. `key_env` names an env var holding the key. Since v1.2 the
mothership points every route at its provider gateway with `auth: none` and a colony token instead, so
no key enters the colony (§6.5); `key_env` remains for routes set by hand. When any route or non-default
model is configured, the runner starts
a local router on `127.0.0.1` and points Claude Code's `ANTHROPIC_BASE_URL` at it:

- Requests whose JSON `model` matches a route prefix: strip the prefix, send to `base_url` + the request
  path (`/v1/messages`, `/v1/messages/count_tokens`), replace `authorization`/`x-api-key` with the route's
  credential, drop Anthropic OAuth betas from `anthropic-beta`, stream the response back unchanged.
  If the upstream has no `count_tokens`, answer `{"input_tokens": ceil(chars / 4)}`.
- Everything else: forward to `https://api.anthropic.com` with headers unchanged.

## 6.1b Per-task model tiers (mothership)

What §6.1 describes transports a request to a model you named; per-task routing is the part that
names it. With the agent module's `route_per_task` setting on (the default), the mothership picks a
tier for each colony at boot — `low`, `medium` or `high` — from the issue in front of it, with a
pure heuristic over the task's own signals (`crates/colonizer/src/routing.rs`): the issue's labels
(`chore`, `copy`, `docs`, `documentation`, `typo` pull toward `low`; `breaking-change`, `epic`,
`migration`, `refactor` pull toward `high`, and a high label wins over a low one), the task text's
length, its markdown checklist items, how many file paths it names and whether they all sit in one
directory, and whether the colony's sandbox preset is one the harness knows — an unknown preset
never routes down to the cheapest tier. These add to a score, and the score picks the tier. No
model call, no network, no new dependency: the same shape as the other pure decision functions,
`watchdog::decide` and `queue::has_room`.

| Setting | Default | |
| --- | --- | --- |
| `route_per_task` | true | Off: every colony without its own `model_tier` runs on `model` |
| `model_low` | none | Model for the `low` tier: a Claude alias or ID, or `<provider>/<model>`, in the same forms as `model` |
| `model_high` | none | Model for the `high` tier, in the same forms as `model` |
| `route_cost_gate` | true | Price routing down against running the task on `model` directly (below), and keep the colony on `model` when routing down would not actually come out cheaper. Only the rule's own `low` pick is gated; an operator's `model_tier` is an instruction, never second-guessed |
| `route_cost_context_tokens` | 0 | Estimated context tokens (X) the cheaper tier has to reload for a routed subtask; 0 leaves it unknown |
| `route_cost_output_tokens` | 0 | Estimated output tokens (Y) a routed subtask produces |
| `route_cost_reread_tokens` | 0 | Estimated tokens (Z) `model` re-reads afterwards to pick up what the subtask changed |

`medium` runs on the existing `model` setting. A tier whose setting is blank falls back to `model`,
so with neither tier model set nothing changes about which model a colony runs on. Only the
orchestrator model is routed — `subagent_model` and `background_model` are untouched — and the tier
models resolve through the same provider routes as `model` (§6.5's `used_by` counts them). Their
env variables are stripped from the colony's environment once the tier is chosen, so only the
provider actually in use is probed at boot.

Routing down is not always the cheaper run: the cheaper model reloads the task's context from
scratch at its input price, which sometimes costs more than the output saved. When all three token
estimates are set and both models have pricing on file, the cost gate prices the two ways of running
the task — on `model` directly, its output plus the re-read afterwards; routed down, the cheaper
model's context reload and output, plus `model`'s re-read at `model`'s rate — and gates only when
the routed estimate is not strictly cheaper. Until real per-colony token volumes are measured
(Token savings), the estimates are operator-supplied, and all-zero ones skip the gate entirely.

The decision is recorded three ways:

- an `info` line in the colony's session log (`model routing: low tier, score 0: a 180-character
  body, no checklist items, 1 path named`), naming the model when it differs from `model`, the
  cost gate when it kept the colony on `model`, and the rule's tier when an override disagrees;
- a `model_routing` object on the session record — `{point, jev_mode, jev_agrees, floor, tier,
  rule, source, score, reason, model, agent, misroute, signals, jev, cost, sensitivity}`, where
  `point` is always `"routing.tier"`, the first four are §6.1c's, `source` is
  `off`/`rule`/`override`/`jev`,
  `model` is set only when the tier changed it, `agent` names the agent module — the harness — the
  decision was made for, the same name the spend journal's colony rows carry (§6.8), so a routing
  decision can be joined with what the colony went on to spend, `misroute` is true when an operator
  override lands somewhere the rule did not want, `signals` is what the rule read off the issue,
  `jev` is the shadow opinion when one was asked for (§6.1c), `cost` is the gate's estimate — the
  three token counts, both dollar figures, whether routing was worth it and whether the gate fired —
  or `null` when the gate had nothing to say, and `sensitivity` is the strictest class the paths the
  task names classified to (`open`/`standard`/`custom`/`vetted`/`restricted`);
- one JSON line per boot appended to `routing.jsonl` in the mothership's data directory, tagged
  `"kind": "decision"` — the recorded set a future replacement for the heuristic could be evaluated
  against. When a colony that went through routing reaches a terminal state, a second line,
  `{"ts", "kind": "actual", "session", "actual_cost_usd"}`, records what it really spent; joined to
  the boot's `decision` line by `session`, it checks the estimate against the spend.

An operator override is §4's `model_tier` on `POST /api/sessions`; it wins over the rule for that
colony, whether or not routing is on.

A live colony can also switch models without a restart: `set_model` (§2, §4) changes the
orchestrator model for the session's subsequent turns, and the conversation context, microVM and
worktree are kept. The switch lasts for the life of that session, so Stop and Resume derive the
model from the settings and tier again, as before. Subagents that inherit the orchestrator's model
follow the switch; a configured `subagent_model` and the background model are unchanged.
The provider timeouts and context cap the runner derived at boot from the boot models (§6.5) are
not recomputed, so switching to a slower provider or one with a smaller context window is at the
user's risk. A `<provider>/<model>` switch also needs a route the colony booted with: a provider
added after boot has none, and its requests go to Anthropic like any unrouted model.

## 6.1c Jev second opinion (shadow mode)

An optional, default-off external classifier ("Jev", `crates/colonizer/src/jev.rs`) can be consulted
for a second opinion on the tier §6.1b's rule already picked. In shadow mode the opinion is
attached to `Signals`/`Decision` as `jev: Option<JevOpinion>` (tier, model, confidence, an estimated
cost) and recorded alongside the rule's own decision without changing the tier; act mode (below)
lets it pick the tier. Either way `decide` stays the synchronous, pure function §6.1b describes,
with no model and no network call inside it. The
network call happens once, in the async boot path in `boot.rs`, before `decide` runs.

Two settings gate it, and both must be set or nothing happens: `jev_shadow_mode` or
`jev_routing_act` (`claude-code` module settings, default `false`) and a `JEV_API_KEY` in the mothership's own environment. The mothership makes this call
itself, so the key never enters a colony. A missing key, the setting left off, or any
failure of the call all resolve to `jev: None`; none of them is an error, and none of them blocks or
meaningfully slows boot. The whole exchange is bounded by a roughly 1.8-second hard timeout, with a
short retry (up to two retries, three attempts in all, backing off 150ms then 300ms, each attempt
capped at 800ms) only on a 429 or 529 response — anything else
non-2xx, a network error, or a malformed response returns `None` immediately.

What is sent is condensed and metadata-only, never file contents, never the raw task body and never
credentials: the issue title with credential-looking tokens redacted, its labels, a bucketed body size
(`small`/`medium`/`large`/`huge` rather than a character count), the checklist and path counts,
`one_directory` and `known_preset`. The model asked is pinned explicitly (`jev-1.13.0`), never a
"latest" alias, since a second opinion's calibration is specific to one model version and is not
assumed to carry over to the next. Each call's `estimated_cost_usd` is a rough token estimate against
an unverified per-token price, logged so the cost of asking stays visible — it is not metered billing,
and it is not folded into a colony's own routed cost, since the opinion is a classifier call, not
the model the colony runs on.

**Act mode** (issue #583, experimental, default off). `jev_routing_act` (a `claude-code` boolean,
default `false`) lets the opinion pick the tier: it asks Jev exactly as shadow mode does, with or
without `jev_shadow_mode`, and when the opinion's `confidence` is at least
`jev_routing_act_confidence` (a number in 0–1, default `0.8`) `decide` uses Jev's tier instead of
the rule's, with `source: "jev"`. An opinion that lands on the rule's own tier leaves `source:
"rule"`. Below the threshold, with no opinion, or in shadow mode, the rule's tier stands. An
operator's `model_tier` still wins, and routing off still runs on `model`. The #470 cost gate
applies to a low tier Jev picked just as it does to the rule's. Jev's tier is never below the
task's **floor**:

- a task is sensitive when the minimum provider mark its class demands of this org's gateway is
  above `any` (`sensitivity::required_mark` — `vetted` and `restricted` by default, or any class an
  org's overrides raise). A sensitive task's floor is the rule's own tier: Jev may raise it, never
  lower it, since the tier picks the model and so the provider the colony starts on;
- an unknown sandbox preset floors at `medium`, the same clamp the rule applies to itself;
- both: the higher of the two. Neither: no floor.

Every `routing.jsonl` decision row and `model_routing` record carries `point: "routing.tier"`,
`jev_mode` (`off`/`shadow`/`act`), `jev_agrees` (whether Jev's tier equals the rule's, `null` with
no opinion) and `floor` (a tier, or `null`), so Jev-acted decisions can be compared with rule ones
against each session's `actual_cost_usd` and misroute outcomes.

This second opinion and Jev compaction (Token savings) are separate features sharing only the
`JEV_API_KEY`: the opinion reads condensed metadata at boot, while compaction sends conversation
history at each compaction.

**Boot brief (issue #585, shadow only).** A third optional use of the same client, `jev_brief_shadow`
(a `claude-code` boolean, default `false`), asks Jev at boot which of this colony's shared-memory
notes and skill packs are worth loading: up to five picks, one `choice` question per round (each round
offers the remaining candidates plus `none`, and an answer outside them is a miss); an org whose Jev
switch is off is never asked, like every other Jev point. Notes tagged
`house-rule` or `security` are **mandatory** — always loaded, never offered, never dropped — and memory
stays pull-only (§6.2): the picks are recorded, never acted on, so nothing the colony sees changes
while the flag is off. The mothership writes one `pick` row per boot and one `used` row per watched
note read or skill pack touched to `<data dir>/brief_picks.jsonl`, which `bench.mjs brief` grades
([bench.md](../bench.md#grading-jev-brief-picks)); `act` mode waits for the #582 decision layer and a
measured token saving on the bench, so this stays telemetry.

A caveat worth stating plainly: this integration's specific vendor claims — the endpoint, its pricing,
its latency — could not be independently verified while it was built. The design leans on that: with
no key and no flag set, it is inert, so an unverified or even nonexistent vendor causes no harm to a
real deployment. It only ever degrades to "no shadow opinion," every time.
