# Token savings

Part of the [Colonizer protocol](../protocol.md).

Four `claude-code` module settings cut what a colony spends on tokens. All are off by default, and each
works only when the install has what it needs; otherwise the colony boots without it and its log says why.

| Setting | What it does | Needs |
| :--- | :--- | :--- |
| `caveman` (`COLONIZER_CAVEMAN`), `caveman_level` (`lite`, `full`, `ultra`; default `full`) | The agent replies in [caveman](https://github.com/juliusbrussee/caveman)'s compressed style: output tokens | `<COLONIZER_HOME>/vendor/caveman/`, mounted at `/opt/colonizer/caveman` |
| `headroom` (`COLONIZER_HEADROOM`) | Model requests pass through [Headroom](https://github.com/headroomlabs-ai/headroom), which compacts large tool results before the model reads them: input tokens | The Headroom bundle for the machine's architecture, downloaded to `<data>/headroom/<release>` when Headroom is switched on and mounted at `/opt/colonizer/headroom` |
| `rtk` (`COLONIZER_RTK`) | Shell commands go through [rtk](https://github.com/rtk-ai/rtk), which shortens their output before the agent reads it: input tokens | `<COLONIZER_HOME>/bin/rtk`, mounted at `/opt/colonizer/bin/rtk` |
| `jev_compaction` (`COLONIZER_JEV_COMPACTION`), `jev_keep_threshold` (default `0.5`), `jev_preserve_recent` (default `6`) | At compaction, stale tool calls are deleted by Jev score instead of the lossy built-in summary: input tokens on later requests | `vendor/fast-jev-compaction/`, staged by `scripts/fetch-vendor.sh` (which `scripts/install.sh` runs) to `dist/vendor/fast-jev-compaction` and mounted at `/opt/colonizer/jev-compaction`, plus a `JEV_API_KEY` on the mothership |

**caveman.** caveman switches itself on with `SessionStart` and `UserPromptSubmit` hooks that inject its
ruleset and track a per-session level. In a colony the level is the setting, and the runner puts the
ruleset (`skills/caveman/SKILL.md` without its frontmatter) into the system prompt, followed by the one
Colonizer exception: the pull request description, AskUserQuestion questions and options, memory
proposals and code comments stay in plain sentences. Only that file and `LICENSE` are staged, and both are
MIT. caveman's compression engine, proxy and MCP server are BSL-1.1 and are neither staged nor used.

**Headroom.** The runner starts Headroom's proxy on loopback inside the colony and points Claude Code's
`ANTHROPIC_BASE_URL` at it. Headroom forwards to the model router when the colony has provider routes and
to Anthropic when it doesn't, so routing, the Claude fallback and microsandbox's credential swap stay where
they were. What it changes is the size of large tool results: in the bundle's smoke test, a Bash result of
400 JSON log rows reaches upstream 75% smaller, with every error row still in it. How it runs is fixed in
`modules/agents/claude-code/headroom.mjs`:

- `--no-cache`. Headroom's semantic cache answers a similar-enough request without calling the model,
  which an agent must never get.
- `--stateless`. Nothing it would write is worth keeping in a disposable colony.
- No network of its own. Telemetry, update checks, subscription tracking, model downloads and LiteLLM's
  price-map fetch are switched off through its environment.
- No ML compression. Kompress needs a 261 MB model that the bundle doesn't carry, and it is disabled.
- No credential. From the runner's environment it gets `PATH`, `LANG` and the certificate-bundle
  variables (`SSL_CERT_FILE`, `SSL_CERT_DIR`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`), and nothing else. The
  requests it forwards carry the colony's placeholder, which microsandbox swaps for the real credential at
  its TLS edge as before. Those variables are what make Headroom's Python trust that edge; without them,
  requests with no router in front fail with a 502.

It takes 2.5 to 7 seconds to start and 300 to 370 MB of the colony's memory (measured on arm64). If it
exits, or isn't healthy within 90 seconds, the colony runs without it and its log says why.

Headroom is Python (Apache-2.0), and colonies run whatever stack image the sandbox module chose, so it
can't live in the image. Each architecture gets one bundle instead: a standalone CPython 3.13 from
python-build-standalone with `headroom-ai[proxy]` and its dependencies, installed from
`vendor/headroom/requirements.txt` with every hash checked. `.github/workflows/headroom-bundle.yml` builds
it inside Debian bookworm, whose glibc 2.36 matches the colony image, runs `scripts/headroom-bundle/smoke.py`
against it, and publishes a release. `crates/colonizer/headroom.lock` pins each archive by sha256. The pins
are compiled into the mothership, and nothing is downloaded at install.

Saving the agent module in Settings with Headroom switched on starts the download
(`POST /api/headroom/download`). The mothership fetches the bundle for its own architecture (a Mac on Apple
Silicon takes `linux-aarch64`), checks it against the sha256 pin compiled into the mothership, and unpacks
it to `<data>/headroom/<release>`. The archives are 221 MB for aarch64 and 243 MB for x86_64. A colony that
starts before the download has finished runs without Headroom. A new pin is a new download, and earlier
releases stay on disk.

**rtk.** The runner registers an in-process `PreToolUse` hook on `Bash` that runs `rtk rewrite <command>`.
Exit 0 or 3 with output replaces the command (3 is a rewrite rtk's ask rules flag; the colony's own
permission handling still applies), and anything else (1 for no rtk equivalent, 2 for a deny rule, rtk
missing, or no answer within 2 seconds) runs the command unchanged. The hook returns only
`updatedInput`, never a permission decision, so it can't allow what `delegate = enforce` denies. Rewritten
commands call `rtk`, so `/opt/colonizer/bin` is put first on the agent's `PATH`. Read, Grep and Glob
don't go through the shell and aren't rewritten.

`scripts/build-rtk.sh` builds rtk from the source pinned in `vendor/vendor.lock` as a static musl binary
inside a `rust:1-alpine` microVM, like `colonizer-agentd`, and skips the build when that source is already
built for the machine and the build mode. Upstream's aarch64 Linux release is linked against glibc
2.39, newer than the colony image's 2.36, so it would not start in a colony on Apple Silicon. rtk's
telemetry is opt-in and never switched on in a colony.

**Jev compaction.** When the context fills, Claude Code's built-in `/compact` summarizes the
transcript — lossy rewriting. Jev compaction instead deletes stale tool calls by Jev score and keeps
the rest verbatim: nothing kept is rewritten, and when scoring fails or the reduction is too small
it falls back to the built-in summary. It runs as a Claude Code plugin, staged from
github.com/tamaratran/fast-jev-compaction (MIT) pinned by git commit in `vendor/vendor.lock` and
unpacked by `scripts/fetch-vendor.sh` (which `scripts/install.sh` runs) to `dist/vendor/fast-jev-compaction` — never `dist/plugins`, so it
can't be picked as a skillset. A git pin rather than npm: the npm tarball ships only the library,
without the hooks and plugin manifest. The Agent SDK can observe a compaction but can't replace the
compacted messages, so driving the library from the runner would have meant rewriting Claude Code's
session file and resuming — more code and fragile. The runner hands the plugin directory to the SDK,
sets `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1` in Claude Code's environment only while the switch is on
(function hooks need Claude Code ≥ 2.1.274; session init warns on anything older, or when the plugin
didn't load), and forwards `jev_keep_threshold` (default `0.5`: what scores high enough to keep) and
`jev_preserve_recent` (default `6`: newest messages never touched, alongside the first message, which is always kept) as plugin config. The mothership
gives the colony its own `JEV_API_KEY` — the same key the Jev routing second opinion uses — as
microsandbox secret `TYPESAFE_API_KEY` for `api.typesafe.ai`, so the guest sees only a placeholder.
Without the staged payload or the key the colony runs without it and its log says why. With the
switch on, the runner also turns on Claude Code's debug log (a scratch file in the VM's temp dir),
because headless Claude Code keeps the plugin's verdict only there. Each
compaction is reported on the event stream, e.g. `Compaction (auto, 182344 → 41210 tokens): kept
41/180 messages, no summary (…)`. It composes with both other savings: rtk shrinks command output at
tool time, Jev prunes the transcript at compaction, Headroom compresses each request in flight — and
Jev talks to `api.typesafe.ai` directly, never touching `ANTHROPIC_BASE_URL`, so no two proxies
contend.

With it on, the colony's conversation and tool-call history — file paths, command output — leaves
the machine for TypeSafe (`api.typesafe.ai`) at each compaction: an exception to "code doesn't leave
the machine". TypeSafe bills that traffic directly; it doesn't pass through the Colonizer gateway,
so it's invisible to `model_usage` and colony cost. Org workspaces override only the models and skillsets,
so it can't be switched on per org. Read TypeSafe's data terms before using it on private repos.

**A skill pack is not a token setting.** The four settings above are switches the harness controls
and the harness can turn off again. A vendored skill pack changes the agent's instructions, and the
pack that would plausibly cut the most tokens — [ponytail](../skill-packs.md#ponytail), which makes
an agent stop at the first rung that holds before writing code — is **off by default**, switched on
one install at a time, for exactly that reason. Its cost-saving figures are upstream's own
benchmark, published by the ponytail project; Colonizer has not reproduced them here, and nothing on
this page should be read as a Colonizer measurement. What would, and has not been run yet, is the
[bench protocol for a skill pack](../bench.md#measuring-a-skill-pack).

**Jev visibility ladder.** Each applied compaction pass is also measured (#475): the runner reports
every chunk's keep/drop decision with Jev's own relevance scores on a `jev_ladder` event (§2), and
the harness logs one `decision` row per chunk to `<data>/jev_ladder.jsonl`. When the agent later
re-issues a tool call equivalent to one a decision was about — same tool, same canonical input —
the harness logs a `reread` row naming the original, once per decision: the evidence that the chunk
was actually needed again. Together the two rows give the keep/drop decisions a precision and
recall against that ground truth, logged in the colony's harness log as it accumulates. The
measurement is shadow-only: nothing in it changes what compaction keeps or drops, and the ledger
lives in the data dir, kept across colonies like `routing.jsonl`, because the point is a later
bench-wide report. Known limitation: a pass the plugin computed but did not apply
(`applied: false`, the fallback path) is not measured — nothing was removed from the transcript,
so there is nothing for a later call to be a reread of.
