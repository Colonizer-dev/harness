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
`/v1/messages` (`crates/colonizer/src/openai.rs:21-25`, a roughly 900-line translator). Supporting a
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
(`crates/colonizer/src/sessions.rs:664-668`, `crates/colonizer/src/sandbox.rs:56-58`); the in-colony
router forwards unrouted models to Anthropic with Claude Code's own headers
(`modules/agents/claude-code/router.mjs:178-179`). The gateway only ever attaches a provider key, as
`x-api-key` or `bearer` (`crates/colonizer/src/gateway.rs:224-234`).

### If it is ever revisited

The shape would be an agent module that bundles `codex app-server` and owns its own sign-in — following
the Claude Code module, where the subscription credential never reaches the gateway — not a new
credential class in the gateway. Conditions that would change the decision:

- OpenAI documents third-party access to ChatGPT plans, or opens OAuth client registration; or
- the gateway gains a Responses API wire for other reasons.
