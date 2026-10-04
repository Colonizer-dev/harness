# Colonizer decisions

A record of the things Colonizer has decided against or deferred, so the reasoning is not lost. An
entry here is not a vow: each one states what would change it.

---

## ChatGPT subscriptions are not a Colonizer credential

Decided 2026-09-17 · status: stands ([#30](https://github.com/Colonizer-dev/harness/issues/30))

Colonizer will not support using a ChatGPT subscription as a colony credential. OpenAI-compatible
providers take an API key. Revisit if the conditions at the end change.

### It is a different API, not a different credential

ChatGPT sign-in tokens — what the Codex CLI's sign-in flow obtains — are honoured by
`chatgpt.com/backend-api/codex/responses`, the Responses API, not by `api.openai.com/v1/chat/completions`.
The gateway's `openai` wire translates the Anthropic Messages API to Chat Completions only, and only for
`/v1/messages` (`crates/colonizer/src/openai.rs:22-26`, a translator of about 1,100 lines). Supporting a
ChatGPT plan means a second translator, Anthropic Messages to Responses API items and events. That is a
parallel project, not a follow-up. The provider catalog carried a `codex` preset until this entry; with
`wire: "openai"` it sent Chat Completions requests to an endpoint that only answers the Responses API, so
it could not work.

### It is not a legal refusal

OpenAI's current Terms of Use (effective 1 January 2026) contain no clause reserving ChatGPT access to
OpenAI's own interfaces. The nearest clauses prohibit programmatically extracting data or Output, and
reverse engineering — neither is aimed at a user driving their own subscription. The reports that
third-party harnesses reusing the Codex OAuth client were cut off do not hold up on inspection: the one
first-hand report ([openai/codex#14215](https://github.com/openai/codex/issues/14215)) is an HTTP 403
whose error code is `unsupported_country_region_territory`, with no statement from OpenAI. Meanwhile
OpenAI documents [`codex app-server`](https://developers.openai.com/codex/app-server) as a supported way
to embed Codex in a third-party product, Codex owning the ChatGPT OAuth flow, and third-party tools ship
ChatGPT-plan sign-in (Roo Code, January 2026; OpenClaw, announced by OpenAI's CEO in May 2026).

What has no documented path is rolling your own OAuth client against Codex's hardcoded `client_id`;
there is no registration programme for that. Unclear and unsupported, then, not prohibited. The decision
rests on the engineering argument above, not on this.

### The Claude analogy is mechanically wrong

Claude subscription auth never touches the gateway. `claude_login.rs` drives `claude setup-token` on the
mothership; the token is injected as a microsandbox `--secret` scoped to `api.anthropic.com`
(`crates/colonizer/src/boot.rs:981-985`, `crates/colonizer/src/sandbox.rs:62-67`); the in-colony
router forwards unrouted models to Anthropic with Claude Code's own headers
(`modules/agents/claude-code/router.mjs:178-179`). The gateway only ever attaches a provider key, as
`x-api-key` or `bearer` (`crates/colonizer/src/gateway.rs:961-966`, `credential_header`). Mothership-side judging holds to
the same rule ([#143](https://github.com/Colonizer-dev/harness/issues/143)): the `autonomy` module
sends a model to the provider configured for it and spends that provider's key — a plain id resolves
only where a configured provider sits on Anthropic's API — and the mothership's Claude login is
never spent answering a colony's questions.

### If it is ever revisited

[`modules/agents/codex`](../modules/agents/codex) now drives `codex exec` headlessly on an OpenAI API
key (a `CODEX_API_KEY` colony secret, not a plan). For ChatGPT-plan access the shape is unchanged: an
agent module that bundles `codex app-server` and owns its own sign-in — not a new credential class in
the gateway. Conditions that would change the decision:

- OpenAI documents third-party access to ChatGPT plans, or opens OAuth client registration; or
- the gateway gains a Responses API wire for other reasons.

---

## Features register in one hand-kept `features.rs` list, not an `inventory`

Decided 2026-10-04 · status: stands ([#824](https://github.com/Colonizer-dev/harness/issues/824))

Every feature used to add itself to central lists that all parallel pull requests edit and conflict
on: `.merge(...)` lines in `server::api_routes`, `server::start_tasks`, the one big `match` in
`api_tokens::classify`, and `RULES`/`KINDS` in `activity.rs`, plus the single `routes.snap`. Each
migrated feature now carries a `Feature` descriptor beside its handlers — its routes, scoped-token
rule, activity rules, kinds and background work — and lists itself with one sorted line in
`features::ALL`. `server`, `api_tokens` and `activity` read that list, and the route table is one
snapshot per source module under `crates/colonizer/routes/`. This entry records why the list is
written by hand rather than discovered at link time with `inventory`/`linkme`.

### No link-time dependency to own

`inventory` and `linkme` are link-time registration crates. Both add a real dependency to the
supply chain and to `Cargo.lock` (and `inventory` pulls `ctor`, a proc-macro), for a problem that is
one line per feature. The mothership is the thing that boots colonies; keeping its dependency graph
plain is worth more than saving that line. It also keeps the whole assembler readable without
knowing how a linker collects static initialisers.

### It works on every target, tests included

Link-time registration relies on the linker collecting distributed static sections. That needs the
right target support and flags, and it can silently collect nothing (a binary with no references to
the objects) — the classic `inventory` "it works in tests, not in the release binary" footgun. A
`const ALL: &[&Feature]` is ordinary data Rust compiles the same way everywhere, so the tests and
`UPDATE_ROUTE_SNAPSHOT` see exactly what the shipping binary does.

### It is explicit and greppable

A person (or a colony) can read `features.rs`, `main.rs` and one snapshot per module and see the
whole surface. `grep -r 'features::ALL'` finds every consumer. Link-time registration makes "where
does this feature's token scope come from?" a runtime question.

### Conflicts only ever arise between alphabetical neighbours

The lists were already alphabetical and the issue accepts a sorted central list. A migration is one
line in `features::ALL` and its `mod` line in `main.rs`, and two features that are not neighbours
never touch the same line. The `mod` line is needed either way: `inventory` still needs the module
compiled in, so it saves the `ALL` line only, not the `main.rs` one.

### If it is ever revisited

The failure this trades away is merge conflicts in `features.rs` and `main.rs`'s `mod` block when
two features are close alphabetically. If those become frequent enough that resolving them costs more
than a link-time crate (naming a feature `zzz` to dodge the neighbour is not a fix), revisit with
`inventory` — and move `ALL` behind the same `Feature` shape, so only the collection changes.
