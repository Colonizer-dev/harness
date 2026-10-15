# Data refresh

Part of the [Colonizer loops](../loops.md).

A colony loop for repositories whose data files have to be re-checked against outside sources — a
product catalogue, a table of exchange rates, a statistic a README cites — and whose changes should
carry proof. Each run fetches the sources that are due, keeps what it fetched as evidence with its
sha256, confirms every change with a second fetch before keeping it, and opens one pull request that
shows its work, labelled by the repository's own policy script. It is a template, not a built-in
loop: **Loops** → **New loop** → the **Refresh data files from their sources** chip (or the CLI
below), then edit the Inputs lines at the top of the prompt to name your repository's files and
commands. The chip turns **Needs GitHub** on ([loops.md](../loops.md#what-the-colony-can-do)) — a
run reads `/colonizer/github/issues.json` and files `source-broken` issues through the host — and
defaults to every day at 05:00 your local time.

## The inputs

The template's prompt opens with the inputs it was created with, and those lines are the ones to
edit:

| Input | Default | The run uses it to |
| --- | --- | --- |
| Sources file | `data/sources.json` | choose the due sources and keep `verified_on` and `failures` up to date |
| Extract command | `npm run extract -- <id>` | read the fetched evidence and print one source's changed values as JSON |
| Validate command | `npm run validate` | accept the applied changes; a change it rejects is dropped and explained in the pull request |
| Policy command | `npm run --silent refresh-policy` | decide the pull request label: it reads the change set on stdin and prints `auto` or `review` |
| Evidence directory | `evidence/` | keep what was fetched, one folder per source |
| Max failures | 3 | when a source's `failures` count reaches it, the loop reports the source as broken |

The sources file is a JSON list; `method` is `http` (fetched with curl) or `browser` (a headless
browser, for JS-rendered pages), and `cadence` or `volatility` says when a source is due:

```json
[
  { "id": "fx-rates", "url": "https://example.com/rates.json", "method": "http", "cadence": "daily", "verified_on": "2026-10-09", "failures": 0 },
  { "id": "supplier-catalogue", "url": "https://supplier.example.com/catalogue", "method": "browser", "volatility": "weekly", "verified_on": "2026-09-14", "failures": 0 },
  { "id": "member-count", "url": "https://example.org/about", "method": "http", "cadence": "monthly", "verified_on": "2026-08-30", "failures": 2 }
]
```

## What each run does

1. Loads the sources file and picks the due shard: the entries whose cadence or volatility class
   makes them due since their `verified_on`. A run started with `only` sources (below) covers
   exactly those ids, due or not.
2. Fetches each source by its `method`, saving what it fetched to `evidence/<id>/<UTC timestamp>.<ext>`
   with its sha256 beside it as `.sha256`, and runs the extract command over it.
3. Re-fetches and re-extracts a changed source at least 2 minutes later and keeps the change only
   when the two results agree; an unconfirmed change stays out of the pull request and is listed
   under "Not confirmed" in its description.
4. Applies the confirmed changes and runs the validate command, dropping a change it rejects and
   saying why. The description carries a change table (source, field, old → new, % change for
   numbers) and, per changed source, its URL, fetch time, evidence path with sha256, and the
   extractor version (what the extractor prints, else `git log -1 --format=%h` of its files).
5. Runs the policy command and writes `data-refresh:auto` or `data-refresh:review` — whatever it
   printed — to `/harness/out/pr-labels`, which the harness labels the pull request with
   ([mothership-api.md](../protocol/mothership-api.md)).

Sources that were checked and did not change get their `verified_on` set to today, and those bumps
ride in the same pull request as the changes — or, when nothing changed at all, in one
`verified on <date>` pull request. Never more than one pull request per run.

When a source cannot be fetched or extracted, its `failures` count in the sources file goes up by
one (back to 0 after a success) and the error is listed in the description. When `failures` reaches
the maximum (3 unless you changed it) the loop reports the source with the finding tool, titled
exactly `source-broken:<id>`, with the error and the last evidence path — see the limits below.

## Run one source now

An external change detector does not have to wait for the nightly slot: `loop run` takes `--only`
(repeated or comma-separated), and the API takes the same list in the run-now body
([protocol/loops.md](../protocol/loops.md)):

```sh
colonizer loop run loop_x1 --only fx-rates --only member-count
```

```sh
curl -sS -X POST https://<mothership>/api/loops/loop_x1/run-now \
  -H "Authorization: Bearer <token>" -H "Content-Type: application/json" \
  -d '{"params":{"only":["fx-rates","member-count"]}}'
```

The body is optional — without it a run-now behaves as always and covers every due source. `only`
takes 1–100 ids of 1–100 characters (`A`–`Z`, `a`–`z`, digits and `._:/-`); anything else answers
**400**. The run's brief names the ids and tells the colony to work only on those sources. While the
loop's previous run is still live the answer is **409** — the run is not queued, so retry after it
ends.

## Limits

- **One run at a time.** A firing (or run-now) that lands while the previous run is still going is
  skipped or refused with **409**, never queued; a fixed schedule simply tries again at its next
  slot ([loops.md](../loops.md#when-it-runs)).
- **Labels are best effort.** `/harness/out/pr-labels` holds at most 10 labels of 50 characters
  each, none containing a comma (a comma would split at `--add-label`); at publish the mothership
  creates any that are missing and applies them to the pull
  request, and a label that cannot be created or applied is logged and dropped — it never blocks
  publishing. Only loop colonies can write the file.
- **`source-broken` issues go through the finding tool** ([findings.md](../protocol/findings.md)):
  the mothership validates each one host-side before anything reaches GitHub, a colony files at most
  5 findings, and issues are deduplicated by exact title — a `source-broken:<id>` issue that is
  already open is not filed twice; when `/colonizer/github/issues.json` lists it, the colony
  comments on it instead.
