# The MCP server

`colonizer mcp` serves this harness to MCP clients over stdio: list colonies (agent sessions
working GitHub repositories in microVMs), read one's status or pending question, answer it, stop
or resume it, or launch a new one. It is a client of the mothership's HTTP API — the same
`--host` and token resolution as every CLI command, so everything [cli.md](cli.md) says about
naming a mothership and choosing a token applies here too. The protocol travels on stdin/stdout;
the server's own notes go to stderr: one startup line naming how many tools it serves and at what
scope, for example `colonizer mcp: serving 6 of 10 tools at scope read`.

## The tools

The tool set is the token's scope, resolved once at startup from the mothership
(`GET /api/tokens/self`):

| Tool | Scope | Parameters | Returns |
| :--- | :--- | :--- | :--- |
| `list_colonies` | read | `org?`, `status?` | JSON array, newest first: `{id, repo, status, issue, title, pr_url, cost_usd}` per colony |
| `colony_status` | read | `id` | the colony's detail — status, branch, pull request, cost, and what it is doing now |
| `colony_question` | read | `id` | the question the colony is waiting on: `question_id`, `risk`, and the questions with their options; plain text when nothing is pending |
| `colony_pr` | read | `id` | one sentence: the pull request URL and state (checks state when known), or that there is none yet |
| `colony_diff` | read | `id`, `stat_only?` | everything the colony changed against its base branch: `{id, repo, base, files, added, removed, diff, truncated}`; `stat_only` omits the diff text |
| `search_repo_map` | read | `repo`, `query` | `{repo, revision, query, components}`: the components of the repository's architecture map matching the query — a label, id, type, source path, or a file under one — each with the connections that touch it; a tool error when the repository has no map yet |
| `answer_colony` | operate | `id`, `answer` | a confirmation. The answer matches the pending question the way `colonizer answer` does: an option's 1-based number, its whole label, or free text |
| `stop_colony` | operate | `id` | a confirmation; the microVM goes away, the worktree is kept for a later resume |
| `resume_colony` | operate | `id` | a confirmation with the colony's new status |
| `launch_colony` | launch | `repo`, `issue?`, `task?`, `model?`, `autopilot?`, `allow_duplicate?`, `queue_behind_holder?`, `allow_epic?` | `{id, status}` of the new colony. `autopilot` means the mothership opens the pull request by itself when the agent finishes cleanly; omitted, the install's setting decides. `allow_duplicate` starts a second colony on an issue another colony already holds, `queue_behind_holder` waits behind the holder instead, and `allow_epic` starts one on an epic; each defaults to false, so the launch guard refuses with a 409 as usual |

## Scopes

- **The owner token defaults to `read`.** It may do everything, but an MCP client's default
  exposure is watching. `colonizer mcp --scope read|operate|launch` raises the session's scope.
- **A scoped token serves its own scope.** `--scope` can only lower it, never raise it — the
  mothership would refuse the extra calls anyway.
- **Withheld tools are hidden.** A tool the scope does not reach is not in the client's tool list
  at all, and calling it is refused as a tool-not-found error — a client never sees a tool every
  call of which would fail. The mothership still enforces the scope on every call.

## Errors

A call that could not do its job comes back as a tool error — the MCP `isError` bit — carrying
the reason in its text: the mothership's own refusal (403 naming the scope, 404 for an unknown
colony, 429 at a token's cap) or the transport failure reaching it. Nothing here turns a refusal
into a protocol failure the client cannot read. If the token's standing cannot be read at
startup, `colonizer mcp` exits instead of serving a tool set that would only be refused.

## Installing it

Claude Code, on the machine that runs the mothership (the owner token is read from the local
install, and the session is read-only):

```sh
claude mcp add colonizer -- colonizer mcp
```

The same, allowed to answer, stop and resume colonies:

```sh
claude mcp add colonizer -- colonizer mcp --scope operate
```

With a scoped token in the server's environment (the flags go before the server name):

```sh
claude mcp add colonizer --env COLONIZER_TOKEN=col_… -- colonizer mcp
```

Cursor, in `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "colonizer": {
      "command": "colonizer",
      "args": ["mcp"],
      "env": { "COLONIZER_TOKEN": "col_…" }
    }
  }
}
```

Codex, in `~/.codex/config.toml`:

```toml
[mcp_servers.colonizer]
command = "colonizer"
args = ["mcp"]

[mcp_servers.colonizer.env]
COLONIZER_TOKEN = "col_…"
```

opencode, in `opencode.json`:

```json
{
  "mcp": {
    "colonizer": {
      "type": "local",
      "command": ["colonizer", "mcp"],
      "environment": { "COLONIZER_TOKEN": "col_…" }
    }
  }
}
```

A mothership on another machine takes the same global flags as any client command — put them
before the subcommand in the args array (`["colonizer", "--host", "mothership.tailnet:7878",
"mcp"]`); `COLONIZER_TOKEN` reaches the server through each client's environment block, above.
What the mothership must allow for remote callers is in [cli.md](cli.md).

**Use a scoped token for agents.** Mint one per client (`colonizer token create …`) with the
least scope that client needs, bounded by org and repo limits and, at `launch`, by a concurrency
cap and a daily budget. The tool list then shows exactly what that client may do, `--scope`
cannot raise the ceiling, and nothing an agent does holds the owner token.

## The cockpit chat's tools

The cockpit's Chat (and Spotlight's inline answers) reach this same surface in-process, scoped to the
signed-in cockpit user, through tools the model calls on Anthropic-wire models (`crates/colonizer/src/chat_tools.rs`).
Every tool is a **read** or a **write**:

- **Reads** (`list_colonies`, `colony_status`, `colony_question`, `list_issues`, `list_loops`, `list_providers`,
  `model_assignments`, `list_orgs`, `recent_activity`) run at once. Results drop key-like fields and are redacted.
- **Writes** (`stop_colony`, `resume_colony`, `publish_colony`, `answer_colony`, `launch_colony`, `move_to_front`,
  `move_to_back`, `set_priority`, `switch_models`, `run_loop_now`, `apply_update`) never run when the model calls them.
  The call becomes a pending approval: the tool, its arguments, a plain-language summary, the diff from the API's
  `dry_run` where it has one (`switch_models`), and the blast radius (colonies, orgs, repos).
- `GET /api/chat/approvals[?chat=]` lists approvals; `POST /api/chat/approvals {tool,args,chat?}` holds a write the
  cockpit itself proposes (Spotlight's Do rows); `POST /api/chat/approvals/{id}` with `{"decision":"approve"|"edit"|"reject","args"?}`
  settles one **exactly once** (a second decision is a 409). Each decision is an activity-log line, `chat.approve` or
  `chat.reject`, naming the chat message that proposed it. Writes are rate-limited to 10 a minute.
- Refused outright: any tool or argument that names a secret (Settings → Secrets is the only way), releasing a
  security hold (resume, answer and publish on a held colony), and any org that is switched off.
