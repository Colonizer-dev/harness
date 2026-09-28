# The evolver

A colony fails and the reason sits unread in what the run already recorded. The evolver reads those
records — bench runs, red-team reports, findings ledgers, colony transcripts — groups the failures
into classes, holds one class still long enough for someone to write a prompt change against it, and
keeps the change only when a rerun of the bench beats the baseline. `scripts/evolve.mjs` is the
first slice of that loop ([#310](https://github.com/Colonizer-dev/harness/issues/310)): a subcommand
per stage — diagnose, propose, evaluate, retain — plus `list`, `approve` and `reject` for the queue.
It never launches colonies, never calls the mothership's API, never edits repository files and never
opens a pull request: it reads logs and writes proposal files into one output directory, and
everything past that — running the bench, judging a proposal, applying a diff — is yours.

## The loop

```sh
node scripts/evolve.mjs diagnose --bench bench-after.json --findings ~/.local/share/colonizer/sessions/<id>/findings.jsonl --out diagnosis.json
node scripts/evolve.mjs propose --classes diagnosis.json --class module:claude-code/questions --module claude-code --constant SYSTEM_PROMPT_APPEND --text new-prompt.txt
node scripts/evolve.mjs evaluate --proposal <out>/<id>.md --baseline bench-before.json --candidate bench-after.json
node scripts/evolve.mjs retain --proposal <out>/<id>.md
node scripts/evolve.mjs list
node scripts/evolve.mjs approve <id>
```

`COLONIZER_DATA_DIR` (or `~/.local/share/colonizer`) is where proposals go by default:
`<data dir>/evolver/proposals`.

**Diagnose** clusters confirmed failures into classes. It reads bench run files (`bench-<label>.json`),
red-team synthesis reports (`redteam-report.jsonl`, protocol §6.7, `validated` lines only) and
findings ledgers (`findings.jsonl`, protocol §6.6, findings that reached `validated`, `filed` or
`merged` folded by title; `rejected` and `blocked` stay out). A class is `{id, surface, kind, count,
evidence}`: the surface is the agent module that ran the failing tasks (`module:claude-code`) or the
file a red-team defect names, and the kind comes from a fixed vocabulary read off the bench's own
failure strings — `questions`, `scope`, `hidden-check`, `tests`, `no-pr`, `timeout` — plus
`reward-hack` for a trajectory the monitor flagged, `redteam` for a synthesis defect, `defect` for a
ledger finding, and `other` when nothing matches. Class ids are `${surface}/${kind}`, so they are
stable across runs, and evidence entries link back to the bench task, session and label — or the
defect title, severity and hunters — that put them there. `--data <dir>` joins the colony
transcripts' question and tool-error counts into bench evidence.

**Propose** turns one class into a candidate. The replacement text is written by you (or a model you
run) — the diagnosis is the brief, not the draft — and `propose` does the mechanical part: it checks
the prompt-only guard, locates the one file in `modules/agents/<module>/` that declares the constant,
computes a unified diff without touching the working tree, records the harness commit as
`base_commit`, then writes `<out>/<id>.md` plus the raw `<out>/<id>.diff` and prints the exact
reproduction commands. Only constants whose names end in `_PROMPT` or `_PROMPT_APPEND` are accepted —
that is the whole safety envelope for this stage: one string constant, in one agent module, per
candidate.

**Evaluate** also runs nothing: you check out `base_commit`, run the baseline, `git apply` the diff,
run the candidate, and hand both run files over. The candidate must cover exactly the baseline's
task ids, or `evaluate` refuses. What comes back is a scorecard — per task, pass against pass and
cost against cost, where a pass is a task that passed *and* was not hacked (an unaudited pass
counts, a hacked one never does) — plus the totals: score, cost delta in dollars and percent, and
the token delta per category. The sums reuse bench.mjs's own `summarizeRun`, so the two tools cannot
drift apart.

**Retain** stamps the verdict, and the scorecard is kept whatever it says:

- a cost increase over `--max-cost-increase` (default 0.10, i.e. 10%) is **discarded**;
- a single task regressing from pass to fail is **flagged**, even when the average improves — never
  silently retained;
- a score that is not strictly better is **discarded**;
- when both runs were audited by the trajectory monitor, its `compareProposal` verdict is the last
  word: a proposal whose clean rate does not improve — a widened [gap](trajectory-monitor.md)
  included — is **flagged**: the raw score went up, and the audit says it may have been bought;
- otherwise **retained**.

With several candidate runs each is scored against the same baseline — one run of one task is one
sample, and the bench has no seeds, so pin the commit, the task ids and the held-out set version
that `propose` records — and the proposal is retained only when every run agrees; the per-run
verdicts are kept in the file.

`list` shows the queue (id, class, status, verdict). `approve <id>` — refused for anything not
`retained` or already `rejected` — marks it approved and prints the `git apply` command: from there
it is an ordinary, human-authored pull request. `reject <id> --reason "…"` records a no. Neither
touches the repository.

## The proposal file

One Markdown file per candidate, human-reviewable and machine round-trippable. The title line
carries the id and status; **Claim** names the class and lists its evidence; **Diff** holds the
unified diff; **Reproduce** the commands, the base commit, the task ids and the held-out version;
**Scorecard** the per-task table and totals once evaluated; **Verdict** the retain decision with its
reasons; **Review checklist** the four items to settle before the diff becomes a pull request — docs
for the changed prompt, pins, the audit question of what the new text promises the colony
([audit.md](audit.md)), and the bench gain confirmed by hand. The machine record lives in a trailing
JSON fenced block that `parseProposal` reads back, fenced with more backticks than any run inside
the file, so a diff that itself contains a fence cannot break it.

## Limits

- `propose` reads and writes one string constant, and does not understand the code around it; a
  replacement text that contradicts what the runner does with the constant is exactly as wrong as it
  would be by hand.
- The bench measures the tasks it has; a prompt tuned against four visible tasks can overfit them,
  so run the [held-out suite](bench.md) and the [trajectory monitor](trajectory-monitor.md) when the
  bench runs.
- `evaluate` trusts that the candidate run really had the diff applied; the proposal records how to
  reproduce it, and nothing checks that it was.
- No proposal has been retained yet. The first surface is prompt-only, one module (`claude-code`),
  deliberately narrow; widening to module defaults and settings waits until three retained proposals
  have reproduced on re-run.
