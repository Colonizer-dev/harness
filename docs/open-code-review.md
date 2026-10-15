# open-code-review

A colony's diff gets a machine review before the human one: the `ocr` CLI of
[alibaba/open-code-review](https://github.com/alibaba/open-code-review) (Apache-2.0) runs on the mothership
host, over the colony's diff, right before it is published (issue #1274). The binary is not shipped with the
app and never fetched at install time — the mothership downloads a pinned release when someone asks in
Settings. Beyond off there is exactly one mode, **shadow**: findings are recorded — colony-log lines, a
JSON report in the session directory — and nothing changes; whatever ocr says, the publish, the pull
request and the colony are untouched.

## Installing the binary

`crates/colonizer/open-code-review.lock` pins one release — v1.13.0 today — compiled into the mothership.
The platform is the *host's* (the review runs host-side), OS and architecture both: Linux and macOS, amd64
and arm64; anything else reports `unavailable` and refuses a download with 409. A review runs only when
the pinned release is installed — after a pin bump, download again.

| | |
| --- | --- |
| Where | Settings → open-code-review: the Download button, or `POST /api/open-code-review/download` — starts the download or joins one running, returns at once |
| Status | `GET /api/open-code-review`: `{name, release, installed_release, state, path, bytes, total, started_at, finished_at, error}`, `state` being `idle`, `downloading`, `unpacking`, `installed`, `failed` or `unavailable`. Owner token |
| Integrity | The lock's sha256 is checked on the stream; the exec bit goes on the verified partial file and one rename moves it into place, so the final name only ever holds a checksum-passing binary |
| Lands at | `<data>/open-code-review/<release>/ocr` — today `<data>/open-code-review/v1.13.0/ocr` |

## Modes, per org

An org setting, not an install setting — it decides what every colony of that org spends on reviews — and
off unless the operator opted in.

- **`open_code_review`** — `off` (the default; absent and `null` are the same) or `shadow`. Any other
  value, `on` included, is refused at save time: `on` does not exist yet.
- **`open_code_review_model`** — the model reviews run on, a `provider/model` string. Absent or blank
  follows the install's cheap route (below); refused at save time if it contains whitespace or exceeds
  120 characters.

Both are set in the cockpit's org settings, `PUT /api/orgs/{org}`, or `orgs.json`; a save from an older
build keeps them, a named `null` clears them.

## When it runs

On the publish path, and only there: after verification, immediately before the publish — the one moment
the diff is final and still on disk. The org's mode must be `shadow`, the pinned binary installed, a model
routable; each miss costs one colony-log line at most (mode `off` runs nothing, a missing binary an info
line pointing at Settings, an unroutable model a warn) and the publish carries on regardless. The diff is
snapshotted synchronously under a 30-second budget — past it, one warn line and no review — then the review
is spawned: nothing waits on ocr. A review that outruns the ten-minute ceiling is killed.

## What the review sends to the model

ocr reviews a snapshot, not the colony's worktree: a fresh throwaway git repo under
`<data>/open-code-review/`, two commits (`base`, then `head`) holding the changed paths — the working tree
and untracked files against the merge base of `origin/<base>` and HEAD, the tree the publish commits. The
directory is deleted when the run ends; leftovers older than an hour — a crashed run's `.shadow-*` dir, an
interrupted download's `*.part` — are swept before the next snapshot or download. What never reaches it:

- **Credential-like file names** at any depth — the dotenv family (`.env`, `.env.*` minus the
  `.example`/`.sample`/`.template`/`.dist` templates), `.envrc`, `.netrc`, `_netrc`, `.npmrc`, `.pypirc`,
  `.git-credentials`, `.pgpass`, SSH private keys (`id_rsa`, `id_dsa`, `id_ecdsa`, `id_ed25519`).
- **Paths the path policy masked** from the colony — the same recorded list the publish checks
  ([path policy](path-policy.md)).
- **Binaries, symlinks and noise**: files that are not UTF-8 text, empty files, files over 1 MiB, symlinks
  (never followed — the link is not reviewable content, and following one could read outside the worktree),
  changed paths beyond the first 500 — the summary line says how many were left out — or that git could
  not have named (absolute, `..`, into a `.git`).

Every text that does go in — base side from git, head side from disk — passes through the same secret
redactor the colony logs use, so a key quoted in code leaves as `[REDACTED:…]`. A new file has no base
side; a deleted file has no head side. A `.gitignore` the diff carries cannot hide anything (`add -f`).

The CLI runs with a cleared environment — only `PATH`, a throwaway empty `HOME`, and the `OCR_LLM_*`
variables — so it reads none of its own config (`~/.opencodereview/`) and none of the ambient
`ANTHROPIC_*`. The model is the org's `provider/model`, routed through the provider gateway with its wire
protocol, or, unset, the install's cheap route: the agent module's summary model, else its
`subagent_model`/`model_low`/`background_model` — which may be the operator's own Anthropic key straight
to `api.anthropic.com`. The key reaches the process as `OCR_LLM_TOKEN` and is never written anywhere; a
provider that needs no key gets none. No route at all is one warn line and no review.

## Where the findings land

- The colony log (`sessions/<id>/harness.jsonl`, the cockpit's colony view): one summary line —
  *open-code-review (shadow): N findings — x critical, y high, …* — then one line per finding, `path:line
  [category · severity] content`, clipped to 500 characters.
- The session directory: the whole JSON report, every finding's text redacted again on the way out, at
  `sessions/<id>/open-code-review.json`. A finding too odd to parse is dropped, not fatal; a failed run
  is one warn line.

## Turning it off

Set the org's `open_code_review` to `off` (or `null`) and no review runs again; there is nothing else to
undo — shadow changed nothing. To take the tool off the machine, delete `<data>/open-code-review/` — the
installed release, and any snapshot a running review has not cleaned up.

## Not yet

Planned follow-ups of #1274:

- **`on` mode** — the colony fixing what ocr found, a capped number of rounds, findings posted as line
  comments on the pull request.
- **A merge-steward recheck**, so an updated branch is reviewed again rather than carrying the first
  run's findings.
