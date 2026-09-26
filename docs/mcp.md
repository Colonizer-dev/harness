# The MCP server

`colonizer mcp` serves this harness to MCP clients over stdio: list colonies (agent sessions
working GitHub repositories in microVMs), read one's status or pending question, answer it, stop
or resume it, or launch a new one. It is a client of the mothership's HTTP API — the same
`--host` and token resolution as every CLI command, so everything [cli.md](cli.md) says about
naming a mothership and choosing a token applies here too. The protocol travels on stdin/stdout;
the server's own notes (one startup line naming how many tools it serves and at what scope) go to
stderr.

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
| `search_repo_map` | read | `repo`, `query` | the repository's architecture map searched for components matching the query — a label, id, type, source path, or a file under one — with the connections that touch each hit; a tool error when the repository has no map yet |
| `answer_colony` | operate | `id`, `answer` | a confirmation. The answer matches the pending question the way `colonizer answer` does: an option's 1-based number, its whole label, or free text |
| `stop_colony` | operate | `id` | a confirmation; the microVM goes away, the worktree is kept for a later resume |
| `resume_colony` | operate | `id` | a confirmation with the colony's new status |
| `launch_colony` | launch | `repo`, `issue?`, `task?`, `model?`, `autopilot?` | `{id, status}` of the new colony |

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

Claude Code:

```sh
claude mcp add colonizer -- colonizer mcp
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
