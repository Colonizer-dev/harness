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

**`.git` is not a bind.** The git admin dir is mounted read-only at its own
host path at boot already, so `.git/config` and `.git/hooks/` are beyond the
colony's reach without a bind of their own; they are listed above for the
record and for reporting.

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
is held back as above.

## Not yet covered

- **Runtime per-access violation events.** A read of a masked file is blocked
  by the mount, but nothing reports *attempted* access while the colony runs;
  the only report is the publish-time changed-path log above.
- **Paths created mid-session in nested checkouts** are caught only at
  publish: the placeholders and binds exist for the paths that existed at
  boot.
- **Per-org overrides.** The three settings are global (the sandbox module);
  an org cannot carry its own lists yet.
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
