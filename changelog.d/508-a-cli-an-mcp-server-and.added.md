- **A CLI, an MCP server, and scoped API tokens.** The `colonizer` binary is now also a client:
  `launch`, `list`, `status`, `logs`, `ask`, `answer`, `stop`, `resume` and `pr` drive a mothership
  here or across a tailnet (`--host`, `COLONIZER_TOKEN` or `--token-file`), with exit codes a
  script can read, shell completions and a man page. `token create/list/revoke` mints scoped API
  tokens — named, least-privilege keys ordered `read` < `operate` < `launch`, with org and repo
  limits, a concurrency cap and a daily dollar budget, stored as a SHA-256 hash and accepted as a
  Bearer header only; what a token launches is marked to the agent as external input, and the
  activity log records it as `token:<name>`. And `colonizer mcp` serves the harness to MCP clients
  over stdio, its tool set following the token's scope. See [docs/cli.md](docs/cli.md) and
  [docs/mcp.md](docs/mcp.md). ([#508])

[#508]: https://github.com/Colonizer-dev/harness/issues/508
