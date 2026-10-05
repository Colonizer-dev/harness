# Decisions inbox

Part of the [Colonizer protocol](../protocol.md).

The cockpit's "Decisions" section (issue #1036, [cockpit.md](../cockpit.md#decisions)): open issues
waiting on the operator's decision and pull requests that need a person, from every org the
operator opted in. Served by the mothership's `decisions` feature (`crates/colonizer/src/decisions.rs`);
the opt-ins and the answered issues are saved in `<config_dir>/decisions.json`.

| Route | What it does |
|---|---|
| `GET /api/decisions` | `{count, decisions: [decision], prs: [pr], orgs: [{org, enabled, default_on, explicit, polled_at, error}], writes_blocked, writes_blocked_reason, paused_until, poll_minutes}`. Answered from the mothership's cache and its own records; it never calls GitHub. `count` is `decisions` plus `prs`. Owner only: the cards span every opted-in org. |
| `PUT /api/decisions/orgs/{org}` | `{enabled: bool \| null}`: opts the org in or out; `null` returns it to its default. Answers the `GET` view. `400` for a name that is not a GitHub org. Owner only. |
| `POST /api/decisions/answer` | `{id: "owner/repo#n", choice, note?}`: posts exactly one comment, `Decision (maintainer): <choice>` with the note below it, then removes the `needs-decision` label when the issue carries it. Answers `{id, comment, label_removed, label_error}` — a label that would not come off is reported, never retried, so the comment is never posted twice. `400` for an empty choice, a choice over 500 characters or a note over 4000; `404` when no open card has the id (answered already, or never found); `409` while the same card is being answered, or while `COLONIZER_NO_EXTERNAL_EFFECTS` blocks writes; `502` when GitHub refused the comment. Owner only. |
| `POST /api/decisions/pr-action` | `{id, action: "rerun" \| "redo" \| "dismiss"}`, for an action the card lists; `publish` answers `400`, as it goes to the colony's own `POST /api/sessions/{id}/publish`. `dismiss` forgets a `commits_not_merged` card → `{dismissed: id}`. `rerun` re-runs the failed jobs of the Actions runs behind the pull request's failing checks (at most five runs) → `{rerun: [run ids]}`; `409` when no failing check came from an Actions run. `redo` dispatches the merge-train loop's redo colony, with the pull request as its reference → `{colony}`; `409` when a redo colony (or a newer pull request on the same issue) already exists, or one was dispatched for this pull request before. `409` while writes are blocked. Owner only. |

A `decision` is `{id, org, repo, number, title, url, question, options: [string], option_details:
[string], recommended, context, source: "label" | "body" | "comment", labelled, more, updated_at}`;
empty `options` means the card offers free text, `option_details` holds one description per option
(empty when it has none), `recommended` is the recommended option as it appears in `options` or
`null`, `context` is the first paragraph of the issue's `## Why` (up to 240 characters) or `null`,
and `more` counts further questions in the same text. A `pr` is `{id, org, repo, number, title, url,
colony, reason, why, actions}`; `id` is the pull request's URL, or `colony:<id>` for a colony held
before it published; `reason` is one of `commits_not_merged`, `policy_hold`, `needs_redo`,
`conflicted`, `red_ci`, `review_requested` and `awaiting_merge`, in the order the inbox lists them;
`actions` lists `rerun`, `redo` and `dismiss` where they are safe, and `publish` on a `policy_hold`
card whose colony's autopilot holds a publish it can still make: the cockpit sends it to
`POST /api/sessions/{id}/publish`, whose click is the approval, never through the inbox.

## What counts

**A decision** is an open issue, in an opted-in org, that carries the `needs-decision` label, or
whose body or latest comment has a line starting `Open decision:` or `Decision needed:` (any case,
Markdown quote, heading, bullet or bold around it allowed) or a decision heading. A heading `Decision needed`, `Decision`, `Open decision`
or `Decisions needed` (a trailing colon or parenthetical allowed) opens a decision section, which
ends at the next heading. The text after the marker is the question; when the marker stands alone,
the question is the first blockquote before any list item (`>` and bold taken off), or else the next
line. Options are the list items (`-`, `*`, `+`, `1.`, `1)`) under an `Options:` line after the
question, up to ten; without one, the items in the section that open with a bold label (`- **A:
stacked (recommended).** More…`), the label being the option (trailing `.`/`:` off) and the rest its
description; without those, a question of the form `X, or Y?` or `X or Y?` with no question word in
front, one "or" and at most eight words on each side offers `X` and `Y`. Otherwise the card is free
text. The recommended option is the one whose label says `(recommended)` (taken out of the label),
or the one a `Recommended default: **X**` or `If no answer comes, build X` line in the section names
(`X` alone or the start of an option's label). A marker in the latest comment wins over the body's. A body marker stops counting once the
latest comment starts `Decision (maintainer):` or the operator answered it from the inbox; the label
counts until it is removed — except when the latest comment is the operator's own answer from the
inbox and only the label removal failed, so a card never invites a second answer.

**A pull request needs a person** when:

- `commits_not_merged` (issue #1075): the merge train or its loop squash-merged it, and its branch's
  tip afterwards had commits after the merged head. The branch is kept; the card stays until it is
  dismissed. Read from `merge-train-loop.json`'s `commits_not_merged`, so it shows although the colony
  is merged.
- `policy_hold`: its colony carries a control-defeat flag, its autopilot is holding the publish
  because a secret was redacted from its pull request description, or its publish was refused.
- `needs_redo`: the merge-train loop's mechanical rebase conflicted, or its resolve colony gave up —
  unless a redo colony was already dispatched for it.
- `conflicted`: it is behind or conflicts with its base and the auto-rebase could not finish.
- `red_ci`: its checks failed and the merge-train loop's last report is not re-running them as a
  known flake (`flaky_checks`); the loop's own reason is quoted when it has one.
- `review_requested`: GitHub says a review is requested from the signed-in account.
- `awaiting_merge`: its checks are green and neither the merge train nor its loop drives the
  repository, so nothing merges it.

## How GitHub is read

Only the issue search goes out, through the mothership's GitHub layer (`github::gh_get`): one query
per org, `org:<org> is:open ((is:issue AND (label:needs-decision OR "Open decision" OR "Decision
needed")) OR (is:pr AND review-requested:@me))`, with `advanced_search=true`, as a conditional
request (`If-None-Match`; a `304` costs no rate limit). A matching issue with comments has its latest
comment read, conditionally, only when its `updated_at` changed, at most ten per poll. A background
tick each minute searches at most one org, and each org at most every five minutes. A failing search
doubles that org's wait, up to four hours. A `403` or `429`, an abuse answer or a secondary rate
limit pauses every org: 15 minutes, doubling with each push-back in a row up to four hours, and reset
by the next search that succeeds. Everything else is read from what the mothership already holds —
its colonies, `merge-train-loop.json` and the publish module's merge-train settings.

**Opt-in.** An org is on by default only when it is switched on as a workspace and already has
colonies; `PUT /api/decisions/orgs/{org}` overrides that either way. An org that is off is never
searched and its cards are not shown.

**Writes.** Nothing is posted, labelled, re-run or dispatched without the operator's click on a
card. With `COLONIZER_NO_EXTERNAL_EFFECTS` set the inbox is read-only: the search still runs,
`writes_blocked` is true with the reason, and the three write routes answer `409`.
