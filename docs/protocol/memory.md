# 6.2 Shared memory (runner ⇄ mothership)

Part of the [Colonizer protocol](../protocol.md).

Approved notes are mounted read-only in every colony:

```
/colonizer/memory/global/  MEMORY.md  notes.json  notes/<id>.md
/colonizer/memory/org/     MEMORY.md  notes.json  notes/<id>.md     (the colony's GitHub org)
/colonizer/memory/repo/    MEMORY.md  notes.json  notes/<id>.md     (the colony's repository)
```

**Memory is pulled, never injected (issue #766).** No note text goes into a colony's system prompt or
its first message. The prompt carries one fixed line saying the memory tools exist, and the agent pulls
what it needs through MCP. `COLONIZER_MEMORY_DIR=/colonizer/memory` tells the runner memory is enabled,
and the runner serves four tools:

| Tool | Input | Answer |
|---|---|---|
| `memory_briefing` | `topic?` (words that must all appear) | A short, sourced summary: one entry per line as `[scope/kind] Title: summary`, then `source:` (colony, repo, commit, and whether a person reviewed it; `the maintainer` for your own notes; `promoted from …` for fleet-wide notes) and the entry's id. Repo entries first, newest first. At most 12 entries |
| `memory_changes` | `since?` (ISO 8601) | What was added, and what was revoked or removed, since the colony last called either tool (or since `since`). A revoked entry is named so the agent stops relying on it |
| `memory_search` | `query` | Snippets from the note files, every term matching |
| `memory_propose` | `scope`, `title`, `content`, `kind?`, `confidence?`, `tags?` | Emits a `memory_proposal` event; nothing is written inside the colony |

Both new tools read `notes.json`, the structured store the mothership rewrites the moment a note is
approved or revoked, and wrap their answer in a `<shared-memory>` frame that names the content data to
verify, not instructions. A note's title and summary are flattened to one line, so a note cannot start
a line of the answer of its own.

**Kinds.** Every entry is one of `plan`, `decision`, `file_change` (a note about a change to specific
files), `failure`, `architecture` (an architecture note) or `convention`. Notes stored before kinds
existed, and proposals that name none, are `convention`; an unknown kind is refused.

`MEMORY.md` is an index (`- [Title](notes/<id>.md) — first line`) kept for people and older runners. Every
note a colony wrote is labelled before its title: `(from a colony, reviewed)` once a person approved it,
`(from a colony, not reviewed)` when stored with review off, and `(from a colony)` for notes from before
that was recorded. Note files written from then on carry the matching `> Written by a colony…` line under
the heading. Proposing emits a runner event:

```jsonc
{"type":"memory_proposal","origin":"orchestrator","scope":"repo","title":"Run tests with --locked","content":"markdown…","tags":["tests"],"kind":"convention","confidence":0.9}
```

`origin` names who asked: `orchestrator`, a `subagent:<name>` or a `background:<name>`. Only the
orchestrator proposes — the runner's `PreToolUse` hook refuses the tool for any agent with an
`agent_id`, and the mothership refuses any proposal whose origin is not the orchestrator's
(`memory_read_only`, logged in the colony's transcript) before it touches a store, so with the `mem0`
provider a refused proposal is never sent upstream. An event without `origin` — a runner from before
the field existed — is read as the orchestrator. The matrix and where it is enforced are in
[architecture](../architecture.md#shared-memory-access).

The mothership records it as a pending proposal and broadcasts `{"type":"memory_proposed","proposal":{…}}`
(no `seq`) on the colony's event stream. Approved proposals become notes and appear in every colony's
mount immediately. With `require_review` off, a `repo` note is stored at once with `source.reviewed: false`
(`status: "approved"` in the broadcast); `org` notes are always queued. A repo note stays in its own
repository's scope: no other repository's colony mounts it.

**Promotion to fleet-wide memory.** A colony cannot put a note straight into `global` memory. A `global`
proposal is recorded as a sighting of a fleet-wide *candidate* (same kind, same title words), one per
repository, in `candidates.json` on the mothership. A candidate is queued for review as a global note only
once colonies in **at least two distinct repositories** proposed it with **`confidence` of at least 0.8**
(absent counts as 0), and only once. It is always reviewed, whatever `require_review` says, because it
reaches every colony (issue #376). The queued note's `source.promoted_from` lists every qualifying
sighting's colony, repository and commit.

**Provenance and revocation.** Every note a colony proposed keeps `source.session_id` (the colony),
`source.repo` and `source.commit`, the commit its worktree was at, read on the mothership rather than
taken from the agent. `POST /api/memory/notes/{id}/revoke?scope=&key=` with an optional `{reason}`
removes a note from `notes.json`, the index and its note file, so every later `memory_briefing` goes
without it and `memory_changes` reports it revoked. What it was and where it came from are kept in
`revoked.json` beside the store, outside every colony's mount. With mem0 the note is deleted upstream and
the next boot goes without it.

**Where approved notes live** is the memory module's provider. `files` keeps them on the mothership and
mounts each scope directory. `mem0` keeps them in a [mem0](https://mem0.ai) project through its Platform
API (v3), and the runner side is identical:

- Proposals queue on the mothership either way. mem0 only receives a note once it is approved (or a repo
  note stored with review off), written with `infer: false` and `immutable: true` so mem0's extraction
  model never rewrites or later consolidates text a human reviewed.
- Each scope is a mem0 `user_id` (`colonizer:global`, `colonizer:org:<org>`, `colonizer:repo:<owner>/<repo>`)
  and every memory carries `app_id: "colonizer"`. Colonizer's own fields (`colonizer_id`, `scope`, `key`,
  `title`, `tags`, `source`, `created_at`) ride in `metadata`. Listing and deleting are filtered on both, so a
  mem0 project shared with other tools is safe to point at.
- At boot the mothership lists the colony's three scopes from mem0 and writes them into the colony's session
  directory in the layout above. The colony never talks to mem0 and never sees the key, and a resume
  rewrites the layout rather than keeping deleted notes. `MEMORY.md` is ordered by mem0's relevance to the
  task (the issue title, the instructions, then the issue body, not the full prompt).
- If mem0 cannot be reached at boot, the colony still starts, with an empty layout and a `warn` in its log.
  An approval that cannot reach mem0 fails with `502` and the proposal stays in the queue; with review off, a
  repo note that cannot be stored is queued for review instead of dropped.
