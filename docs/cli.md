# The `colonizer` CLI

One binary, two jobs. With no subcommand, `colonizer` starts the mothership, exactly as it always
has: it serves the cockpit and the API on `COLONIZER_BIND` (default `127.0.0.1:7878`) and runs the
colonies. The subcommands are everything else: a few run against this machine (`version`,
`update`, `open`, `login-item`, `telemetry`, `completions`, `man`), and the rest are clients of a mothership already running somewhere —
here or across a tailnet (`launch`, `list`, `status`, `logs`, `diff`, `ask`, `answer`, `stop`,
`resume`, `pr`, `map`, `token`, `mcp`). Settings still come from the environment, never flags — every
`COLONIZER_*` variable is in [install.md](install.md).

## Which mothership, which token

Every client command takes the same two global flags, before or after the subcommand. The local
commands accept them too, and ignore them:

- **`--host HOST[:PORT]`** — the mothership to talk to. The default is the local one
  (`COLONIZER_BIND`, else `127.0.0.1:7878`). A bare host name gets the default port 7878; a host
  with a port of its own is kept as spelled; a bare IPv6 literal is bracketed (`::1` dials as
  `[::1]:7878`). A URL (`http://…`) is refused — the mothership serves plain HTTP.
- **`--token-file PATH`** — a file holding the API token (the value itself, one line). The token
  resolution order is `COLONIZER_TOKEN`, then this file, then the local install's own token at
  `<config_dir>/api-token` — written by the mothership's first start; the client commands only
  read it, never mint one (the local `open` does create it on a first run). With none of the
  three there is nothing to prove yourself with and the command says so.

`colonizer open` is local on purpose: it reprints the sign-in link and opens a browser on this
machine, always with the local token and `COLONIZER_BIND`, whatever `--host` or `--token-file`
say. The mothership on another machine cannot be opened from here. The cockpit itself is described
in [cockpit.md](cockpit.md).

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

On this machine:

```sh
colonizer version             # what this build is, and whether it is a release (also --version)
colonizer update              # install the newest release against a running mothership, restart into it
colonizer update --force      # also over a development build, or a build newer than the latest release
colonizer open                # reprint the cockpit sign-in link and open it in a browser
colonizer login-item enable   # start the mothership at login (status, disable too; disable never stops one)
colonizer telemetry show      # anonymous usage reporting (on, off; no network, no daemon needed)
```

Colonies — the ids are what `list` and the cockpit show:

```sh
colonizer launch owner/repo "migrate the auth tests"   # a colony working the task you give it
colonizer launch owner/repo --issue 42 --no-autopilot  # one issue; you open the pull request yourself
colonizer list --org acme --status running             # colonies this token may see, newest first
colonizer status abc123                                # where it stands, what it costs, what it is doing now
colonizer logs abc123 -f                               # recent events; -f streams until Ctrl-C
colonizer diff abc123                                  # everything the colony changed, as a unified diff
colonizer diff abc123 --stat                           # per-file +/- counts instead of the diff text
colonizer ask abc123                                   # the question it is waiting on, options numbered
colonizer answer abc123 1                              # by option number, label, or free text
colonizer stop abc123                                  # the microVM goes away; the worktree is kept
colonizer resume abc123                                # back on the kept worktree where it was left
colonizer pr abc123                                    # the pull request URL and state
colonizer map owner/repo                               # the repository's architecture map, as a text outline
colonizer map owner/repo --find login                  # only the components a query matches
```

`launch` takes the repository as `owner/repo`, an optional task as the last argument, and
`--issue`, `--model`, `--subagent-model` and `--autopilot`/`--no-autopilot`. Autopilot is about
publishing, not questions: with it on, the mothership opens the pull request by itself once the
agent finishes cleanly and has written its PR description. The two flags refuse to combine; with
neither, the publish module's `autopilot` setting decides (on by default). Who answers a colony's
questions is a separate setting, the `autonomy` module ([colonies.md](colonies.md#questions-and-who-answers-them)).

`launch` cannot override the launch guards the cockpit can: an issue another colony already holds,
or an epic (an issue with sub-issues), is refused with a 409 (exit 5). Launch those from the
cockpit ([colonies.md](colonies.md#claims-one-colony-per-issue)). `list` filters
client-side: `--org` by repository owner, `--status` by the API's state names, case-insensitively.
`answer` matches its argument against the pending question: a 1-based option number wins, then a
whole-label match case-insensitively, and anything else goes to the agent as a free-text note —
but a bare number that names no option is refused, never silently read as text, and with several
questions pending only a number or label of the first is accepted (`colonizer ask <id>` shows the
rest). An empty `list` or `token list` prints a note to stderr; `--json` prints `[]`.

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

Tokens — the owner token only (see below):

```sh
colonizer token create ci --scope operate --org acme --max-concurrent 2 --budget-usd-per-day 5
colonizer token list
colonizer token revoke tok_x
```

`colonizer mcp` starts the MCP server instead of driving one; it is documented in
[mcp.md](mcp.md).

## `--json`

`--json` is a global flag, like `--host`. It prints machine-readable JSON instead of the human rendering, where a command has one —
what the mothership answered, pretty-printed, for `list`, `status`, `ask`, `stop`, `resume` and
the `token` commands; `logs` prints one JSON event per line, with or without `-f`; `launch` prints
the new colony's record, `pr` a reduced `{id, pr_url, status, ci_state, merged_at}`, `diff` the
diff response object (`{id, repo, base, files, added, removed, diff, truncated}`), `map` the
stored map document — or, with `--find`, the search result — and `answer` echoes the answer body
it sent. Scripts should prefer it to parsing the human columns.

## Exit codes

Exit codes are part of the interface, so a script can tell a typo from a refusal. They are in
`--help` too.

| Code | Meaning |
| :--- | :--- |
| 0 | Everything worked (also `--help`, `--version`, `version`) |
| 1 | Error: the mothership is unreachable, answered with an error no code below names, or the command failed locally |
| 2 | Usage: the arguments name no command this build knows, or a flag's value is refused (a `--host` URL, an unknown `--scope`) |
| 3 | Unauthorized or forbidden: the token is missing or unknown (401), or not allowed (403 — a scope or an org/repo limit refusing) |
| 4 | Not found: no such colony or token (404) |
| 5 | Conflict (409, e.g. `stop` mid-publish); for `ask` and `answer`, a colony that is not asking anything |
| 6 | A launch cap was refused (429): a scoped token's concurrency limit or daily budget |

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

The scopes are ordered, `read` < `operate` < `launch`, each adding to the last:

| Scope | What it may call |
| :--- | :--- |
| `read` | Watch: `GET /api/status`, `/api/version`, `/api/sessions`, `/api/sessions/{id}`, `/api/sessions/{id}/question`, `/api/sessions/{id}/diff`, `GET /api/loops` and `/api/loops/{id}/runs`, the events WebSocket, the `/api/maps/…` reads, and `GET /api/tokens/self` |
| `operate` | Drive colonies that exist: `POST /api/sessions/{id}/answer`, `/stop`, `/resume` |
| `launch` | Start colonies: `POST /api/sessions`, and create, edit, delete and run its own loops (`POST /api/loops`, `PUT/DELETE /api/loops/{id}`, `POST /api/loops/{id}/run-now`) |

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

## Not yet

- A `loop` subcommand — loops live in the cockpit ([loops.md](loops.md)).
- Following a pull request's checks: `colonizer pr` prints the checks state once, when the
  mothership knows it, but nothing waits on it.
- Token management in the Settings UI — the CLI (owner token) and the API are the only ways.
