# The `colonizer` CLI

One binary, two jobs. With no subcommand, `colonizer` starts the mothership, exactly as it always
has: it serves the cockpit and the API on `COLONIZER_BIND` (default `127.0.0.1:7878`) and runs the
colonies. The subcommands are everything else: a few run against this machine (`version`,
`update`, `setup`, `doctor`, `open`, `login-item`, `telemetry`, `hotspots`, `sessions migrate`, `fleet export`, `fleet import`, `completions`, `man`), and the rest are clients of a mothership already running somewhere —
here or across a tailnet (`launch`, `list`, `status`, `logs`, `diff`, `ask`, `answer`, `stop`,
`resume`, `pr`, `map`, `loop`, `redteam`, `token`, `fleet sync`, `mcp`). Settings still come from the environment, never flags — every
`COLONIZER_*` variable is in [install.md](install.md).

## Which mothership, which token

Every client command takes the same two global flags, before or after the subcommand:

- **`--host HOST[:PORT]`** — the mothership to talk to. The default is the local one
  (`COLONIZER_BIND`, else `127.0.0.1:7878`). A bare host name gets the default port 7878; a host
  with a port of its own is kept as spelled; a bare IPv6 literal is bracketed (`::1` dials as
  `[::1]:7878`). A URL (`http://…`) is refused — the mothership serves plain HTTP.
- **`--token-file PATH`** — a file holding the API token (the value itself, one line). The token
  resolution order is `COLONIZER_TOKEN`, then this file, then the local install's own token at
  `<config_dir>/api-token` — written by the mothership's first start; the client commands only
  read it, never mint one (the local `open`, and `update` with no token given, do create it on a
  first run). With none of the three there is nothing to prove yourself with and the command says
  so.

The local commands run against this machine and take none of the client flags. `update` is the
exception: it is a thin client of a running mothership, so it follows `--host` and `--token-file`
like any client command — but it has no `--json`. The rest (`setup`, `doctor`, `open`, `login-item`, `telemetry`,
`version`, `completions`, `man`) refuse `--host`, `--token-file` and `--json` with a usage error
(exit 2), and their `--help` does not list them; `hotspots` and `sessions migrate` refuse the two host
flags and keep `--json`.

`colonizer open` is local on purpose: it reprints the sign-in link and opens a browser on this
machine, always with the local token and `COLONIZER_BIND`. The mothership on another machine cannot
be opened from here. The cockpit itself is described in [cockpit.md](cockpit.md).

**Over a tailnet.** The client half needs nothing from the mothership but reachability. The
mothership side needs three things, or it refuses remote callers:

1. **Somewhere to listen.** `COLONIZER_BIND` is loopback-only by default; set it to a private
   interface IP (a tailnet address, say — never `0.0.0.0`).
2. **A Host header it allows.** The DNS-rebinding guard accepts `localhost`, `127.0.0.1`, `[::1]`,
   the bind address's own host, and the names in `COLONIZER_ALLOWED_HOSTS` (comma-separated).
   Anything else is a 403 saying exactly that — so add the name you will connect by, e.g.
   `COLONIZER_ALLOWED_HOSTS=mothership.tailnet`.
3. **A token.** The CLI authenticates with an `Authorization: Bearer` header, which is header
   auth: it carries no browser, so the same-origin `Origin` requirement that holds for the
   cockpit's cookie never applies. The owner token does everything; a scoped token (below) does
   what its scope and limits allow.

```sh
COLONIZER_TOKEN=col_… colonizer --host mothership.tailnet list
```

## The commands

New to the words? `colonizer help glossary` says what a colony, a settler and the mothership
actually are, in ordinary language. `colonizer help <command>` prints any command's own help.

On this machine:

```sh
colonizer version             # what this build is, and whether it is a release (also --version)
colonizer about               # what Colonizer is built with, from the vendored Factory Zero stack entry
colonizer help glossary       # what this CLI's words mean, in ordinary language
colonizer update              # install the newest release against a running mothership, restart into it
colonizer update --force      # also over a development build, or a build newer than the latest release
colonizer update --check      # say whether a newer release exists and stop; no mothership needed
colonizer setup               # install this version's release over a cargo install (no app assets beside it)
colonizer doctor              # say whether this host can run colonies, and name the fix for whatever is missing
colonizer open                # reprint the cockpit sign-in link and open it in a browser
colonizer login-item enable   # start the mothership at login (status, disable too; disable never stops one)
colonizer telemetry show      # anonymous usage reporting (on, off; no network, no daemon needed)
colonizer hotspots            # the files merged pull requests touched most, over the last 30 days
colonizer hotspots --days 7 --top 5      # a shorter window, fewer files
colonizer hotspots --repo acme/app       # a repository's mirror instead of the current directory
colonizer sessions migrate --to local:/new/data --dry-run   # count what would move, write nothing
colonizer sessions migrate --to local:/new/data             # copy this install's colonies, verified
colonizer sessions migrate --from /old/data --to /new/data  # copy from a store other than the configured one
colonizer sessions migrate --to 's3://colonies/home?endpoint=https://acct.r2.cloudflarestorage.com'  # move to a bucket, and switch
```

`hotspots` reads a git repository on this machine — no mothership — and ranks the files its
merged pull requests touched most often: a header naming the window and the pull requests
counted, then one `<count>  <path>` line per file, busiest first. It counts *distinct* pull
requests, so one pull request editing a file many times counts once, and it drops the
always-touched noise (`CHANGELOG.md`, `changelog.d/`, `Cargo.lock`, the route snapshots `crates/colonizer/routes/*.snap`). `--days`
sets the window (30 by default), `--top` how many files (15). The source is the repository in the
current directory unless `--repo owner/repo` names a mirror in this machine's data dir or
`--git-dir PATH` names a git directory (a mirror, or a worktree's `.git`); the two refuse to
combine. The report is where parallel colonies collide and what to split; `--json` prints
`{days, pull_requests, files}`.

`sessions migrate` copies every colony from the configured session store (or `--from`) into
`--to`, verifies the copy by listing and by every file's SHA-256, and prints what it copied, what
was already there and the source's checksum. Stop the mothership first; the command refuses to copy
a store something is serving. Run it again after an interruption and it copies only what is left.
When the source is the configured store and `--to` is a backend, it switches this install to it
(`<config dir>/session-store.json`); a directory target is a copy, run on with `COLONIZER_DATA_DIR`.
`--json` prints the report. The store, the switch and rollback are in
[session-store.md](session-store.md#migration-and-rollback).

Colonies — the ids are what `list` and the cockpit show:

```sh
colonizer launch owner/repo "migrate the auth tests"   # a colony working the task you give it
colonizer launch owner/repo --issue 42 --no-autopilot  # one issue; you open the pull request yourself
colonizer list --org acme --status running             # colonies this token may see, newest first
colonizer list --parked                                # only parked colonies (--status parked)
colonizer status abc123                                # where it stands, what it costs, what it is doing now
colonizer logs abc123 -f                               # recent events; -f streams until Ctrl-C
colonizer diff abc123                                  # everything the colony changed, as a unified diff
colonizer diff abc123 --stat                           # per-file +/- counts instead of the diff text
colonizer ask abc123                                   # the question it is waiting on, options numbered
colonizer answer abc123 1                              # by option number, label, or free text
colonizer stop abc123                                  # the microVM goes away; the worktree is kept
colonizer resume abc123                                # back on the kept worktree where it was left
colonizer pr abc123                                    # the pull request URL and state
colonizer pr abc123 --wait --timeout 30m               # wait for the checks to settle; exit 7 on failure
colonizer map owner/repo                               # the repository's architecture map, as a text outline
colonizer map owner/repo --find login                  # only the components a query matches
colonizer handoff sess-1 --repo owner/repo             # a session you ran here continues in a colony
colonizer handoff ./session.json --repo owner/repo     # from a txcript document you exported yourself
```

`launch` takes the repository as `owner/repo`, an optional task as the last argument, and
`--issue`, `--model`, `--subagent-model` and `--autopilot`/`--no-autopilot`. Autopilot is about
publishing, not questions: with it on, the mothership opens the pull request by itself once the
agent finishes cleanly and has written its PR description. The two flags refuse to combine; with
neither, the publish module's `autopilot` setting decides (on by default). Who answers a colony's
questions is a separate setting, the `autonomy` module ([colonies.md](colonies.md#questions-and-who-answers-them)).

`launch` takes the same claim and epic overrides the cockpit offers, so a script can get past a
launch guard: `--allow-duplicate` starts a second colony on an issue another colony already holds,
`--queue-behind-holder` waits behind the holder instead (the colony comes back `queued` and starts
when the issue is its own), and `--allow-epic` starts one on an epic (an issue with sub-issues).
Without them, an issue another colony holds or an epic is refused with a 409 (exit 5) that names
the flag ([colonies.md](colonies.md#claims-one-colony-per-issue)). `--package P --advisory A` (always
together) launches a supply-chain fix: a second live colony on the same package and advisory — one
the Packages tab or the supply-chain loop started included — is refused the same way. `list` filters
client-side: `--org` by repository owner, `--status` by the API's state names, case-insensitively.
`answer` matches its argument against the pending question: a 1-based option number wins, then a
whole-label match case-insensitively, and anything else goes to the agent as a free-text note —
but a bare number that names no option is refused, never silently read as text, and with several
questions pending only a number or label of the first is accepted (`colonizer ask <id>` shows the
rest). An empty `list` or `token list` prints a note to stderr; `--json` prints `[]`.

`handoff` continues a session you ran on this machine in a colony: the argument is either a txcript
session id — read here with `txcript export <id> --out <file>` — or a file you exported yourself,
and `--repo` names where the colony works. The document is uploaded, rendered to text and fenced
into the colony's first prompt, and the colony starts from the branch the session recorded unless
`--branch` overrides it and `--title` names it. `txcript` must be on `PATH` (a session id without
it exits 4 with an install hint); a document over 2 MiB is refused before anything is sent. The
answers and limits are the API's ([protocol/sessions.md](protocol/sessions.md#post-apihandoff)).
To go the other way, a colony's conversation is `GET /api/sessions/{id}/handoff`, which writes the
same document for `txcript continue`.

`diff` prints the colony's whole diff: everything it changed against the merge-base with its
base branch — committed and uncommitted tracked edits, plus untracked new files. The colony
need not be live (a stopped colony keeps its worktree), but it must have a worktree: one that
never booted or was cleaned up is a conflict (exit 5). The raw diff goes to stdout, so it
pipes; a truncated diff (the API caps it) is noted on stderr. `--stat` prints one
`+added -removed path` line per file and a total instead.

`map` prints the repository's architecture map — drawn from the cockpit's Map view — as a
compact text outline: title and revision, the components grouped under their boundaries with
their source paths, then the connections. A repository with no map yet exits 4 with a note on
stderr. `--find QUERY` prints only the components matching the query, case-insensitively: a
substring of a label, id, type or source path, or a file under a component's source directory
(`src/auth/login.rs` finds the component whose source is `src/auth`).

`pr --wait` follows a pull request's checks until they settle, re-reading the colony every
15 s — it sees the mothership's view, which the mothership refreshes about once a minute while
checks run. Success, or nothing to wait for (a pull request with no checks), prints the usual
line and exits 0; a failure prints it and exits 7. A colony that ends without opening a pull
request, one whose pull request is merged or closed before the checks settle, or a parked
colony (nothing will publish until it is resumed) exits 1 with a note on stderr.
A settled verdict wins over the status, so a pull request the merge train merged once its
checks went green exits 0. `--timeout DURATION` (`--wait` only; `90`, `90s`, `30m`, `2h`, `1d`
— the unit spellings `loop create` reads) gives up with exit 8 and a note naming the last checks
state; without it, `--wait` waits indefinitely.

Tokens — the owner token only (see below):

```sh
colonizer token create ci --scope operate --org acme --max-concurrent 2 --budget-usd-per-day 5
colonizer token list
colonizer token revoke tok_x
```

`colonizer mcp` starts the MCP server instead of driving one; it is documented in
[mcp.md](mcp.md).

## Red-team runs

A red-team run sends a swarm of hunter colonies at one repository; what a run does is
[red-team.md](red-team.md):

```sh
colonizer redteam start acme/app                                  # armed: starts when no colony is live
colonizer redteam start acme/app --preset security --hunters 8    # the security preset, a full swarm
colonizer redteam start acme/app --now                            # start now (exit 5 while colonies are live)
colonizer redteam list                                            # runs, newest first, with preset and counts
```

`--preset` is `general` (the default) or `security`; `--hunters N` is the swarm size, 1 to 8.
`--model`, `--subagent-model` and `--autofix` mirror the cockpit wizard.

## Loops

Loops are saved prompts that launch a colony on a schedule; what they do is
[loops.md](loops.md). The ids are what `loop list` and the cockpit show:

```sh
colonizer loop list                                              # every loop this token may see, with its next run
colonizer loop create acme/app --name "Triage" --prompt "Triage new issues" daily@09:00
colonizer loop create acme/app --name "Maps" --kind map 14d@03:00   # refresh the map; no prompt needed
colonizer loop run loop_x1                                       # start the next run now (exit 5 while one is live)
colonizer loop stop loop_x1                                      # pause: its settings are kept, nothing runs
colonizer loop start loop_x1                                     # enable a paused or ended loop again
colonizer loop delete loop_x1                                    # delete it; its past colonies stay
colonizer loop run disk-cleanup --dry-run                         # what the built-in disk cleanup would remove
colonizer loop enable disk-cleanup                                # switch it on (disable switches it off)
colonizer loop merge-train show                                  # the built-in merge-train loop: settings and last report
colonizer loop merge-train allow acme/app                        # opt a repository in; `on` switches the loop on
colonizer loop merge-train run --dry-run                         # what it would merge, update, rebase and skip, and why
```

`enable` and `disable` are `start` and `stop` under other names. The built-in **Disk cleanup** loop
([loops.md](loops.md#disk-cleanup)) lists as `(this host)`; `loop run disk-cleanup` prints what it
freed per category, and `--dry-run` what a run would free, path by path, removing nothing. Its
settings are changed in the cockpit or through `PUT /api/loops/disk-cleanup`.

`loop create` takes the repository as `owner/repo` (`owner/*` for a map loop: every repository
of the org), the cadence as its last argument — `30m`/`2h` (every N minutes), `1d`–`7d` (whole
days, still an interval), `14d@03:00` (every N days at a time of day), `daily@09:00`,
`weekly@mon@09:00`, `monthly@15@09:00` or `self` (self-paced: each run names its own next) —
and `--name`, with the prompt from `--prompt` or `--prompt-file PATH` (`-` reads stdin). The
clock times are your local time, stored in UTC exactly as the cockpit's form stores them. The
other flags mirror `launch`: `--model`, `--subagent-model`, `--autopilot`/`--no-autopilot`;
`--max-runs N` ends the loop after N runs, `--retry-failed-runs N` sets how many minutes after a
run that failed for an infrastructure reason it is run once more (default 60; `0` switches the
re-run off) and `--disabled` creates it paused. Cadence ranges
(15 minutes to a week, days 1–365) are the mothership's to refuse, with its message.

`loop stop` and `loop start` are the cockpit's switch: the loop's own settings are sent back
with `enabled` flipped, so pausing keeps everything and re-enabling books the next run from the
cadence. `loop list` shows the cadence in words with its times in your local time, the state
(`enabled`, `paused`, `ended`), when it runs next and the last run's outcome; an empty list prints a note to stderr, and
`--json` prints the raw records everywhere.

## Fleet export and import

A machine's past can travel with it: `fleet export` writes this machine's session history, colony
logs and spend/usage stats into one bundle, and `fleet import` reads a bundle back into a data
dir. Both run locally off the data dir — no mothership needs to be running — and neither reads
anything secret-bearing from the config dir (export touches only its `host_id`, the machine id
the fleet already displays), so no API token, provider key or credential ever leaves the machine.
The bundle format is
[protocol.md, §6.11 Fleet export bundle](protocol.md#611-fleet-export-bundle-687).

```sh
colonizer fleet export                          # colonizer-export-<origin_name>-<YYYYMMDD>.tar.zst in cwd
colonizer fleet export --out /tmp/acme.tar.zst  # a path of your own
colonizer fleet export --no-logs --preview      # what would be written; writes nothing
colonizer fleet import /tmp/acme.tar.zst        # backfill into fleet-imports/<origin_host>/
colonizer fleet import /tmp/acme.tar.zst --preview
```

`export` prints the preview first — per category (`history`, `logs`, `stats`): how many
sessions or files, the time range and the size — then writes the bundle, by default
`colonizer-export-<origin_name>-<YYYYMMDD>.tar.zst` in the current directory. `--no-history`,
`--no-logs` and `--no-stats` leave a category out; an excluded one still shows in the preview,
marked not included. `--preview` prints the preview and writes nothing. `import` prints the same
preview, read from the bundle's manifest, then imports under `fleet-imports/<origin_host>/` with
progress; interrupting it is safe — what landed is kept, and re-running the same file resumes it
and replaces sessions by id instead of duplicating them. With the global `--json` the previews
print as the bundle's manifest. The fleet-join dialog (#686) drives the same format
over the fleet connection, preview → confirm → transfer, so a machine that joins a fleet is
backfilled the same way.

### Fleet sync

A machine that has joined a fleet can push its finished colonies' history to the owner
([fleet.md](fleet.md#history-push)) — once its operator consents; joining alone sends nothing.
`fleet sync --preview` shows what would be sent, `--enable` prints that preview and consents,
`--disable` withdraws consent, `fleet sync` asks the running mothership to drain now, and
`--status` shows where the push stands. None but a plain `fleet sync` or `--enable` sends anything.

```sh
colonizer fleet sync --preview  # colonies, log files and bytes that would go, and what never does
colonizer fleet sync --enable   # print the preview, then consent for this membership
colonizer fleet sync --disable  # stop sending
colonizer fleet sync            # drain now: rows and payloads sent, pending, retired
colonizer fleet sync --status   # consent_required, synced, backoff, unauthorized, removed or error
```

Without consent a plain `fleet sync` fails with the 409 and says how to give it. A manual
`fleet sync` also retries a push that stopped on a 401 or a 403, or is waiting out a
`Retry-After`. With the global `--json` both print the mothership's answer.

## `--json`

`--json` is a global flag, like `--host`. It prints machine-readable JSON instead of the human rendering, where a command has one —
what the mothership answered, pretty-printed, for `list`, `status`, `ask`, `stop`, `resume` and
the `token`, `loop`, `redteam` and `fleet sync` commands; `logs` prints one JSON event per line, with or without `-f`; `launch` prints
the new colony's record, `pr` a reduced `{id, pr_url, status, ci_state, merged_at}`, `diff` the
diff response object (`{id, repo, base, files, added, removed, diff, truncated}`), `map` the
stored map document — or, with `--find`, the search result — `fleet export --preview` and
`fleet import <file> --preview` the bundle's manifest, `hotspots` its `{days, pull_requests,
files}` report, and `answer` echoes the answer body
it sent. Scripts should prefer it to parsing the human columns. The local commands with no JSON
rendering refuse it, like the two host flags above; `fleet export`, `fleet import` and `hotspots`
are local but do have one.

## Exit codes

Exit codes are part of the interface, so a script can tell a typo from a refusal. They are in
`--help` too.

| Code | Meaning |
| :--- | :--- |
| 0 | Everything worked (also `--help`, `--version`, `version`) |
| 1 | Error: the mothership is unreachable, answered with an error no code below names, or the command failed locally |
| 2 | Usage: the arguments name no command this build knows, or a flag's value is refused (a `--host` URL, an unknown `--scope`) |
| 3 | Unauthorized or forbidden: the token is missing or unknown (401), or not allowed (403 — a scope or an org/repo limit refusing) |
| 4 | Not found: no such colony, loop or token (404) |
| 5 | Conflict (409, e.g. `stop` mid-publish); for `ask` and `answer`, a colony that is not asking anything |
| 6 | A launch cap was refused (429): a scoped token's concurrency limit or daily budget |
| 7 | `pr --wait` followed the pull request's checks and they failed |
| 8 | `pr --wait --timeout` ran out of time before the checks settled |

## Completions and the man page

```sh
colonizer completions bash    # or zsh, fish, powershell, elvish — source the script from your shell's rc
colonizer man                 # the man page, rendered to stdout
```

For example:

```sh
echo 'source <(colonizer completions bash)' >> ~/.bashrc                 # bash
mkdir -p ~/.zfunc && colonizer completions zsh > ~/.zfunc/_colonizer     # zsh: put fpath=(~/.zfunc $fpath) before compinit in ~/.zshrc
colonizer completions fish > ~/.config/fish/completions/colonizer.fish   # fish
colonizer man > colonizer.1 && man ./colonizer.1                         # read the man page
```

## Scoped API tokens

A scoped token is a named, least-privilege key for a CLI, an agent or a CI job, so it never holds
the per-install owner token. Managing them is the owner's alone — no scope may mint or revoke a
credential.

- **`token create NAME --scope read|operate|launch`** mints one. Optional limits, each flag
  repeatable where it is a list: `--org ORG` and `--repo OWNER/REPO` (no list means no limit),
  `--max-concurrent N` (the most colonies it may keep unfinished at once) and
  `--budget-usd-per-day D` (the most model spend its colonies may run up per UTC day). The
  plaintext prints once on stdout — store it now; a warning on stderr says the same. Nothing
  reads it back, ever.
- **`token list`** shows metadata only: id, name, scope, org/repo limits (`*/*` when there are
  none) and the day it was made — `--json` adds the exact timestamps, last used among them.
- **`token revoke ID`** takes effect at once; presentations of it stop authenticating.

The cockpit does the same from Settings → API tokens ([cockpit.md](cockpit.md)): it lists, creates
and revokes tokens, and shows a new token's plaintext once, like `token create` does.

The scopes are ordered, `read` < `operate` < `launch`, each adding to the last:

| Scope | What it may call |
| :--- | :--- |
| `read` | Watch: `GET /api/status`, `/api/version`, `/api/sessions`, `/api/sessions/{id}` and its `/question`, `/diff`, `/commits`, `/transcript` and `/files` (listing, archive, content) reads, `POST /api/sessions/{id}/seen`, `GET /api/loops`, `/api/loops/{id}/runs` and `/api/loops/{id}/history`, the built-in loops' `GET /api/merge-train`, `/api/merge-train/loop`, `/api/supply-chain-loop` and `/api/ts-any-loop`, the events WebSocket, the `/api/maps/…` reads, the `/uhp/v1/…` reads, and `GET /api/tokens/self` |
| `operate` | Drive colonies that exist: `POST /api/sessions/{id}/answer`, `/messages`, `/stop`, `/resume`, `/keep`, `/prewarm`, and the UHP cancels `POST /uhp/v1/sessions/{id}/cancel` and `/uhp/v1/responses/{id}/cancel`; and manage its own webhook subscriptions, `GET/POST /api/webhooks` and `DELETE /api/webhooks/{id}`, which receive only events about colonies within its limits ([webhooks](protocol/webhooks.md#subscriptions)) |
| `launch` | Start colonies: `POST /api/sessions` and `POST /uhp/v1/responses`, and create, edit, delete and run its own loops (`POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`) |

A fourth scope, `fleet`, sits outside that ladder and is not creatable here: fleet pairing mints it
for a member ([fleet.md](fleet.md)), and it reaches only `GET /api/hosts`,
`POST /api/fleet/peer/leave`, `GET /api/fleet/policy` (the owner's network floor), the dev-server
preview proxy under `/api/previews/{id}/…`, and the history push's `POST /api/fleet/peer/rows` and
`PUT /api/fleet/peer/payloads/{sha256}`.

Everything else is the owner's at any scope — token management itself, settings, secrets, and
publishing. The enforcement is the same for every client of the API, the CLI included.

A launch token may keep its recurring work in loops: a loop it creates records the token, and each
run is admitted against the token's org/repo limits, concurrency cap and daily budget and marked as
external input, exactly like a colony the token launched by hand; a run a cap refuses is recorded
in the loop's note. Revoking the token ends each of its loops the next time it would run — a
run-now answers 409 — so nothing launches after revocation. The token lists every loop inside its
limits, but edits and runs only the loops it created; an owner's loop reads as unknown to it, and a
map loop (whose runs launch outside any token's caps) is the owner's alone.

- **Org and repo limits** are conjunctions: a colony counts as covered only when an `--org` entry
  matches its repository's owner *and* a `--repo` entry matches its repository, each empty list
  meaning no limit of that kind. A colony or map outside the limits answers **404**, exactly like
  an unknown id — the list is filtered to them, and the token can probe nothing. A launch naming
  a repository outside the limits is **403** before anything starts.
- **The caps** refuse a launch with the reason (exit 6): `max_concurrent` counts the token's
  colonies that are not yet finished — queued ones hold a place — and `budget_usd_per_day` sums
  what its colonies created today (UTC) have spent so far.
- **Storage.** The registry lives at `<config_dir>/api-tokens.json` (0600, beside the owner
  token) and stores only a SHA-256 of each token. The plaintext is returned once, at creation,
  and never again — not by the file, not by the API.
- **Bearer only.** A scoped token authenticates as `Authorization: Bearer` and nothing else; the
  `colonizer_token` cookie stays owner-only, so a browser never holds a scoped token.
- **The agent reads token input as input.** A colony a token launched records the token's id
  (never the secret); its task is marked in the prompt as external input from the token, and its
  answers and messages over the API carry a short `[external input from API token "name"]`
  marker — a description of the task from outside, not the maintainer's voice.
- **In the log.** The activity log records what a token did through the API under the actor
  `token:<name>`.
