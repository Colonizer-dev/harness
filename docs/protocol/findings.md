# 6.6 Findings

Part of the [Colonizer protocol](../protocol.md).

A colony that notices a real problem outside its task (a bug, a security gap, documentation promising
what the code does not do) files it as a GitHub issue instead of fixing it in the pull request.

Runner side. When the mothership sets `COLONIZER_FINDINGS=true`, the Claude Code runner adds an
in-process MCP server `colonizer_findings` with one tool, `finding_file { title, body, evidence }`,
and a system prompt instruction: confirm a finding with a fresh subagent before filing it, and put
how it was confirmed in `evidence`. Only the orchestrator may call it: a `PreToolUse` hook refuses a
call that carries `agent_id`, whatever the delegation mode, and tells the subagent to report the
finding instead. A call emits:

```jsonc
{"type":"finding","title":"llms.txt promises career pages the scanner cannot fetch","body":"markdown…","evidence":"A subagent read model.rs:9-14 and providers/mod.rs:4-19…"}
```

Mothership side. The GitHub token never enters a colony, so filing happens on the host:

- Setting: `publish.settings.file_findings`, default `true`. When it is off the variable is not set, and
  a `finding` event that arrives anyway is ignored.
- Kill-switch: while external writes are blocked (§6.3), a `finding` event is ignored with an `info`
  line in the colony log (`ignored a finding: external writes are blocked
  (COLONIZER_NO_EXTERNAL_EFFECTS), so no issue is filed`). This is checked after the setting above and
  before the finding is parsed or validated, so no validation call is made and nothing is written to
  the ledger.
- Validation: `title` (one line, ≤ 200 chars), `body` (≤ 20 000) and `evidence` (≤ 5 000) are all
  required. A finding without evidence is not filed.
- Cap: at most 5 per colony, counted from `sessions/<id>/findings.jsonl`. A filed finding and a
  duplicate match each use one up; a GitHub error does not.
- Duplicates: open issues are searched by title words (normalised, so no search qualifiers can be
  injected), and one whose normalised title matches exactly means nothing is created.
- Filing: `gh issue create` on the colony's own repository, labelled `colonizer-finding` (created if
  missing, and dropped if the token cannot apply it). The body carries the finding, a "How it was
  confirmed" section and a footer naming the colony and the issue it was working on.
- Every outcome (filed, duplicate, over the cap, rejected, failed) is a line in the colony log. The
  agent is told only that the finding was handed over.

Validation. A finding is filed only after the mothership validates it with a fresh host-side call on
the orchestrator model — the agent module's `model` setting (§6.1). The call is a session of its own:
same model, no shared context with the colony, and nothing it sees is written back into the colony. It
reads the finding's `title`, `body` and `evidence` and answers with a verdict. Nothing reaches GitHub
without a `validated` event, and a `rejected` finding is recorded with its reason and shown, never
dropped — a rejection a human disagrees with stays visible in the ledger and the transcripts.

Each transition is appended to the hunter colony's own `sessions/<id>/events.jsonl` — the same file
`scripts/colony-report.mjs` reads — with the usual `seq` and `ts`, so the colony's report and
transcript show the whole chain:

```jsonc
{"type":"validated","title":"…","severity":"low|medium|high|critical"}
{"type":"rejected","title":"…","reason":"…"}
{"type":"fix_colony","title":"…","session":"<fix colony id>","issue":"<issue url>"}
{"type":"review","title":"…","session":"<review session id>","verdict":"pass|fail","pr":"<pr url>"}
{"type":"merged","title":"…","session":"<fix colony id>","pr":"<pr url>"}
```

These five are host-generated: the mothership appends them to the hunter colony's events.jsonl, the
runner never emits them, and they are not in `docs/agent-events.schema.json` — the runner events stay
the §2 set plus `finding` and `github_action` (§6.12). The publish gate's `verification` event (§6.3,
Autopilot) is host-generated the same way, on the writing colony's own event log. Every transition is
also one line of the ledger,
`sessions/<id>/findings.jsonl`, which the findings endpoints (§4) and the report read: records
`{session, title, state, reason?, severity?, issue?, duplicate_of?, fix_session?, review_session?,
verdict?, pr?, behind_by?}`, `state` one of
`validated|rejected|filed|duplicate|fix_colony|review|automerge|blocked|merged|error`. A
good run is `validated → filed → fix_colony → review → merged`; rejections and failures stay too —
append-only, one line per stage transition, folded by title in the UI.

## 6.6b Colony-to-colony coordination

Parallel colonies of one repository have no channel to each other, so two can append to the same file
and one pull request conflicts the other. Coordination is that channel, over the colony gateway
(`POST /coordinate`), authenticated exactly like recall (§6.5) with the colony's per-colony gateway
token; a token that names no live colony is a `401`. The mothership sets `COLONIZER_COORD_URL` (the
gateway's `/coordinate` URL) and `COLONIZER_COORD_TOKEN` for every colony its gateway token exists
for — like `COLONIZER_IMAGE`, not tied to deja the way recall's are — and the runner exposes one
in-process MCP server, `colonizer_coord`, with four tools (`claim`, `claims`, `send`, `inbox`), each
posting one JSON object; the reply is JSON, and a bad op or input is a `4xx`.

- `{"op":"claim","paths":["crates/colonizer/src/server.rs",…],"reason":"adding a route"}` — merges
  the caller's paths into `sessions/<id>/claims.json` (each with its reason and a timestamp; capped at
  200; trimmed and collapsed to one form — leading `./` or `/`, interior `/./` and duplicate slashes
  gone, a `..` segment refused with a `400`; the reason redacted, since a colony's output is
  untrusted). The reply names every other live same-repo colony whose non-expired claims (idle TTL
  12 h) or pull-request file list overlap the claim — same path, or one a directory prefix of the
  other:

  ```jsonc
  {"ok":true,"colony":"ab12cd34","claimed":["crates/colonizer/src/server.rs"],"total_claims":1,
   "conflicts":[{"colony":"ef56gh78","issue":831,"paths":["crates/colonizer/src/server.rs"],
                 "reason":"changed in its pull request"}],
   "advice":"Another live colony …"}
  ```

  `advice` is present only when `conflicts` is non-empty. Both colonies' logs get a line naming the
  other colony and its issue.
- `{"op":"claims"}` — every live same-repo colony's paths, explicit and derived from its pull
  request: `{ok, repo, colonies:[{colony, issue, self, paths:[{path, reason, at}]}]}`.
- `{"op":"send","to":"<colony id | its prefix | 831 | #831>","text":"…"}` — delivers a redacted
  message (≤ 4000 chars) to one live same-repo colony: `sessions/<recipient>/inbox.jsonl` gets
  `{from, from_issue, text, at}`, and the sender's `sessions/<id>/sent.jsonl` records it. At most 20
  sends per colony per rolling hour (`429` past that). `404` when `to` matches no live colony, `409`
  when it matches more than one.
- `{"op":"inbox"}` — the caller's messages, newest first, at most 50:
  `{ok, colony, count, messages:[{from, from_issue, text, at}]}`.
