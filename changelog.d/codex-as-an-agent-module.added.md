- **Codex as an agent module.** `modules/agents/codex` runs OpenAI's Codex CLI headlessly (`codex
  exec --json`) on the same runner protocol as Claude Code: one process per turn, the first turn's
  thread id resumed into one continuous thread, token totals (codex reports no cost) on each
  `turn_end`, and a `CODEX_API_KEY` colony secret for `api.openai.com` — no ChatGPT sign-in. The
  module is pickable now; nothing stages the `codex` binary into the colony image yet, so a codex
  colony stops at the runner's preflight until the pinned CLI is on the image's PATH.
