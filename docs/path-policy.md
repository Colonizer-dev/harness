# Path policy

A colony runs an agent with write access to a whole worktree. Some files in a
worktree are not the agent's to read — credential files checked in for humans
or tooling — and some it should be able to consult but never rewrite, because
they decide what the agent itself may do. Path policy (issue #300) sorts
worktree-relative paths into two categories and enforces both inside the
microVM before the agent runs a single command.

## The two categories

**Masked** paths are invisible to the colony: reads see an empty file (or an
empty directory), writes land nowhere. The built-in masked paths, all
credential files:

| Path | Why |
| --- | --- |
| `.env` | dotenv secrets |
| `.envrc` | direnv exports, usually secrets |
| `.npmrc` | registry tokens |
| `.netrc` | host logins |
| `.git-credentials` | stored git credentials |
| `.pypirc` | package index tokens |

**Protected** paths stay readable but become read-only: the colony can consult
them and cannot change them. The built-in protected paths, all agent- or
workspace-facing config — the colony's own leash:

| Path | Why |
| --- | --- |
| `.git/config` | git's own config (also covered by the read-only git-dir mount) |
| `.git/hooks/` | git hooks, a code-execution path (ditto) |
| `.gitmodules` | what submodules would fetch |
| `.claude/` | agent settings and hooks |
| `.codex/` | agent settings and hooks |
| `.mcp.json` | tool servers the agent would talk to |
| `.devcontainer/` | container build config |
| `.vscode/` | editor tasks and launch configs |
| `.idea/` | editor tasks and run configs |

Paths are relative to the worktree root. A trailing `/` means the whole
directory; anything else is one file. Matching is by whole path components at
any depth, so `.env` also covers a nested checkout's `vendor/lib/.env`, and a
directory entry covers everything below it.

If a path is in both sets, **mask wins**: it is only masked.

## Settings

All three are sandbox-module settings, arrays of strings, added to the
built-ins:

- **Also mask** (`mask_paths`) — extra paths the colony never sees, for
  credentials the repository carries outside the built-in names.
- **Also protect** (`protect_paths`) — extra paths the colony may read but
  never write.
- **Unmask (opt out)** (`unmask_paths`) — removes an entry from *both* sets,
  built-ins included. For repositories that genuinely ship one of the defaults
  (a test fixture `.env`, say). Every opt-out is logged on the colony at boot,
  so the colony record always shows what the colony could see that a default
  boot would have hidden.

Changes apply to colonies booted after the save, like every sandbox setting.

## Validation

A path entry is refused at save time (Settings → Modules, and `PUT
/api/modules/sandbox`) when it is empty, the worktree root itself
(`/`, `.`), absolute, has leading or trailing whitespace (the guest's `read`
strips it, so it would act on a different path than the one saved), carries a
`.` or `..` component, an empty component, a control character, or `:` or `,`
(which the mount wiring cannot carry). A masked entry under `.git` is also
refused — the git dir must stay readable for the colony to boot at all, and
it is already read-only, so masking it buys nothing. Protecting `.git/*`
entries is allowed. Entries that fail are dropped again when the policy is
resolved at boot, in case modules.json was edited by hand.

## Per-org overrides

An org can carry masked and protected lists of its own, on top of the global
ones: `path_policy.mask_paths` and `path_policy.protect_paths` in the org's
settings (`PUT /api/orgs/{org}`, or `orgs.json` beside `modules.json`). Both
are optional arrays of worktree-relative paths, spelled like the global
settings; `null` or an absent object inherits.

The effective policy for an org's colonies is the union of the built-in
defaults, the sandbox module's settings and the org's entries — and the org
entries also beat the global `unmask_paths`, so an org can re-mask or
re-protect a path the install opted out of. There is no per-org unmask: an org
tightens, never loosens. Mask still wins between the lists.

Org entries are validated like the global ones, with the same rules and the
same 500-character cap, refused at save time (`PUT /api/orgs/{org}`); entries
that fail anyway — `orgs.json` edited by hand — are dropped when the policy is
resolved at boot. A global opt-out an org re-tightened is no longer logged as
an opt-out at boot, since the colony does not see the path after all. The
effective lists, org entries included, are what the boot writes to
`vm/path-policy`.

## How enforcement works

**Host, at boot.** Before the microVM starts, the boot walks each listed path
in the checkout with `symlink_metadata` (no symlink following): a path that is
absent gets an empty placeholder — a file, or a directory for a `/` entry,
with any missing parent directories created — because the guest's bind mount
needs a target. Paths that exist are left untouched on the host. A symlink at
a listed path is resolved while nothing is running: a link that stays inside
the checkout is bound at its target (an in-repo `.env -> config/prod.env`
still masks the real content); one that leaves the checkout, dangles, or
sits under a symlink or a plain file is skipped and named on the boot log rather than
half-enforced. The placeholder list is written before the first placeholder
is created, so a crash cannot leave an empty file no publish knows to remove.
The resolved policy is written to the session's `vm/path-policy` (one `kind
path` line per enforcement action: `mask-file`, `mask-dir`, `protect`; a
directory entry keeps its trailing `/`), and the placeholder list to
`vm/path-policy.placeholders`. Both ride the colony's read-only `/colonizer`
mount. The boot logs one info line — *path policy: masking N path(s), protecting
M; K placeholder(s) in the worktree*, plus any skipped paths — and a warn line
naming every `unmask_paths` opt-out.

**Guest, before the agent.** The boot script reads `/colonizer/path-policy`
before it `exec`s the agent daemon: a `mask-file` entry is covered by a bind
of `/dev/null`, a `mask-dir` entry by an empty read-only tmpfs of an explicit
tiny size, and a `protect` entry by a read-only bind of the path onto itself.
Fail closed on anything unexpected: a missing path, a symlink where the host
wrote a real path (the checkout changed under the policy), an empty path
line, a kind the host does not write, or a lost policy file — each prints a
clear message and stops the boot, since a policy the guest cannot enforce
must not boot into a colony that assumes it was.

**Guest, while the colony runs.** The binds above cover the paths that existed
at boot. A checkout that appears later — a clone, a `git init`, a worktree or
a submodule: any directory below the workspace with its own `.git` — gets its
own enforcement: agentd watches the workspace (#648) and treats every such
directory as a checkout root, binding each policy path relative to that root
as the path appears, with the same mounts the boot script applies. The
workspace root itself is not re-watched, so a `.env` created at the top of the
worktree mid-session is still only reported at publish. A nested checkout's
`.git/config` and `.git/hooks/` are protected too — the host's read-only
git-dir mount covers only the root's, so here the watcher applies the binds
the boot never had to. Like the boot, the watcher never follows a symlink;
unlike the boot, it is best effort: a failed mount is a warn line on the
colony, never a stopped daemon. Two things to know. A read that races the
watcher can still see a just-created masked file — publish still applies. And
a bound path cannot be deleted or renamed inside the guest (the bind holds the
inode), so the agent cannot `rm -rf` a nested checkout that contains a masked
or protected path; each bind that was applied is on the colony log.
Off Linux (a macOS dev build of agentd) there are no bind mounts, so nothing
is enforced: agentd instead polls the workspace about once a second (bounded
in depth and entries) for nested checkouts and writes one warn line per policy
path it finds in one and cannot bind, naming the path.

**`.git` is not a bind.** The git admin dir is mounted read-only at its own
host path at boot already, so `.git/config` and `.git/hooks/` are beyond the
colony's reach without a bind of their own; they are listed above for the
record and for reporting.

**While the colony runs.** The mount is silent by design — a blocked read is
just an oddly empty file — so the runner reports the attempts instead. The
Claude Code runner, and the ACP runner's `fs/read_text_file` and
`fs/write_text_file`, judge each path-taking tool call against the same bind
list the guest booted with (`/colonizer/path-policy`), resolve the path
through any symlink the way the boot resolved its binds, and emit a
`path_policy` event for a hit: a read of a masked path, or a write to a masked
or protected one — a read of a protected path is allowed, so it is not an
attempt. Reporting only: the event carries no decision, and the runner never
blocks; the mount enforced before the report existed (docs/protocol.md §2).
The harness turns each distinct (access, path) into one warn line on the
colony — *path policy: agent tried to read masked `.env` (Read)* — and one
entry in the History log (`colony.path_policy`), never repeating a path within
a run and never carrying more than 100 distinct paths, so a colony circling
against its policy cannot flood either log. Matching is anchored at the
worktree root, which is where the binds sit: `vendor/lib/.env` reads as
unmasked unless a settings entry names it, unlike the publish-time changed-path
log below, which matches at any depth. What is *not* reported: an access that
arrives through a shell command (`cat .env`) never touches a path-taking tool —
that is the exec policy's `secret-paths` rule to refuse, and it does — and an
agent module whose runner is not wired up reports nothing.

**Host, whenever it reads the colony's work.** The placeholders are the
policy's, never the colony's work, and the masked files are not the colony's to
change. Every host-side git that snapshots or stages the worktree works from
the boot's two records (`vm/path-policy` and `vm/path-policy.placeholders`)
and, after its `git add -A`, takes back out of the index:

- any path under a bound masked entry — reset to HEAD's version, so a real
  masked file is carried exactly as the repository has it (never emptied or
  rewritten) and one the repository does not have is never added; and
- any recorded placeholder, or any policy path when that list was lost, that
  HEAD does not carry and that holds no bytes.

The verification snapshot does this in its private temp index, so its
`files_changed` and the fresh checkout it tests never include a placeholder,
and the agent's worktree is untouched. Publish does it before the commit and
logs each held-back path at warn level — *path policy: left `<path>` out of the
commit (masked)* — without file contents.

Git has no per-worktree exclude file: `info/exclude` (what `git rev-parse
--git-path info/exclude` names) lives in the repository's common directory,
shared by every colony on the same repository, so placeholders are not written
there. The per-command hold-back above keeps the exclusion to this colony.

**Host, when the microVM ends and at publish.** Once the microVM is gone (a
stop, a teardown, the start of a publish) the placeholders have nothing left to
be bind targets for, and the still-empty ones are removed from the kept
worktree: the recorded ones, and an empty path at a policy entry when the list
was lost. Never a path HEAD carries — a file the checkout has is the
repository's, whatever its size — never through a symlink, and never anything
with content. (A placeholder cannot be filled through its mask — writes go to
`/dev/null` — so "still empty" means "never real work".) A resume's boot makes
them again. After staging, every staged path matching a masked or protected
entry is also logged on the colony at warn level — *path policy: `<path>` is
masked and changed during the session* — without file contents, once per
colony so publish retries do not repeat the same lines. A changed protected
path is reported and stays in the commit, which is the colony's; a masked one
is held back as above. Staging also refuses a commit that would *add* a
credential file the policy does not name — a `.env.local` or nested `.env` at
any depth, an SSH private key, `.pgpass`, including one reached by a rename —
naming the paths in the error; templates (`*.example`, `*.sample`,
`*.template`, `*.dist`) and paths the repository already tracks are let
through. The flagged files are unstaged and left in the worktree, so deleting
them or adding them to `.gitignore` and publishing again clears the refusal.

## Not yet covered

- **The human's terminal can still unmount.** The colony runs as root inside
  its VM. Since in-guest hardening (#547), the agent and everything it spawns
  run without `CAP_SYS_ADMIN` and under a seccomp filter that answers `mount`,
  `umount2` and the namespace calls with `EPERM`, so the agent cannot undo a
  mask or a read-only bind. The daemon and the cockpit's terminal shell are not
  filtered, so a person typing in that terminal can. The token-file route to
  that terminal is closed for the agent, though: agentd seals `/colonizer/token`
  after reading it (#640), leaving no reader the agent can reach for the
  unfiltered shell's bearer token. See
  [In-guest hardening](architecture.md#in-guest-hardening).
