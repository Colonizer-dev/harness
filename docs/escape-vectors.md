# Escape-vector review checklist

A standing regression list for the colony sandbox. Each documented colony-escape
class is attempted against the real mount / TLS-proxy / mesh setup and given one
verdict: **blocked** (name the exact mechanism, file + config), **open** (filed as
a linked follow-up), or **n/a** (with the reason). This turns the point findings of
[the audit](audit.md) into a checklist every future sandbox change re-runs, so
design-vs-code drift (#232) stays detectable.

Read [boundaries.md](boundaries.md) first: the claimed boundary is the microVM.
In-guest hardening (seccomp, capability drops, `/proc` masking) *narrows what root
can do inside the VM*; it does not replace the VM wall. A vector that only defeats
in-guest hardening without leaving the VM is a hardening-completeness gap, not a
boundary crossing — but it is still recorded here.

## How to use this page

- **KVM-free vs KVM-only.** Properties the harness can assert without a live
  microVM (publish-path neutralisation, egress-rule compilation, gateway
  credential stripping, seccomp BPF semantics, boot-script contents and ordering)
  are covered by `cargo test` and cited per vector. Attempts that need a running
  microsandbox VM (whether a guest `mount(2)` actually fails, whether `-v ...:ro`
  is honoured, whether a bare placeholder 401s at `api.anthropic.com`) get the
  manual procedure at the end of this page and are signed off per release.
- **On a pin move, re-run the affected vectors.** A change to
  `crates/colonizer/images.lock`, `vendor/vendor.lock`, `vendor/claude-code.lock`,
  `vendor/node.lock`, `boot.rs`'s `BOOT_SCRIPT`, `mesh.rs`, `gateway.rs`,
  `egress.rs` or `harden.rs` invalidates the sign-off for the vectors it touches.
- **Every `blocked` names its mechanism** (file + config) so a later code change
  that removes it is visible as drift; every `open` links a follow-up.

## The vectors

### 1. Bind/mount tricks against read-only mounts

*Attempt.* Non-recursive bind over a path carrying sub-mounts (to strip a
`/dev/null` mask); overlay/tmpfs/remount over a protected path;
`mount_setattr`/`move_mount` retargeting of the RO git-object, skill/plugin or
`/colonizer` mounts.

*Result / mechanism.* The runner child cannot call the mount family at all:
`crates/colonizer-agentd/src/harden.rs:76-82` denies `mount`, `umount2`,
`open_tree`, `move_mount`, `fsopen`/`fsconfig`/`fsmount`/`fspick`,
`mount_setattr`, `pivot_root`, `chroot`; `CAP_SYS_ADMIN` is dropped
(`harden.rs:58-65`) and `PR_SET_NO_NEW_PRIVS` set (`harden.rs:294-296`). The RO
mount set is built with `:ro` in `crates/colonizer/src/util.rs:528-536` (separator
injection rejected, tested `util.rs:728-730`) and requested per-mount in
`crates/colonizer/src/boot.rs:712-742`. Path-policy masks are applied in-guest and
**fail closed**: `boot.rs` `BOOT_SCRIPT` binds `/dev/null` over mask files
(`:1259`), an RO tmpfs over mask dirs (`:1262`) and a bind+`remount,ro` over
protected paths (`:1265`); a missing policy file, a symlinked target or any mount
failure exits boot (`:1248,1255,1256,1259,1262,1266,1272`; tested
`boot.rs:1521-1545`).

*Verdict.* **Blocked** (seccomp mount-family denial + `CAP_SYS_ADMIN` drop for the
runner; fail-closed path-policy masks). Caveat: `/workspace` is `rw` by design, and
`:ro` carries no `nosuid`/`nodev`/`noexec` — those rely on msb honouring `:ro`,
which is **KVM-verified** (see manual procedure). Does not cross the VM wall either
way.

### 2. Unmount of masked / protected paths

*Attempt.* `umount2` (and legacy `umount`) on the `/dev/null` masks and RO mounts;
observe whether a secret placeholder or RO git object becomes writable/visible.

*Result / mechanism.* `umount2` is denied for the runner child
(`crates/colonizer-agentd/src/harden.rs:78`; legacy `umount` on x86_64 at
`harden.rs:100`), and `unshare`/`setns` are denied so the masks cannot be undone in
a new namespace; the boot-script ordering guarantee (masks applied before
`exec agentd`, runner then filtered) is asserted at `boot.rs:1277-1279` and tested
`boot.rs:1550-1573`.

*Verdict.* **Blocked** for the runner child. **Open** via the unfiltered PTY: a
root shell obtained through `agentd`'s `/v1/pty` is not seccomp-filtered
(`harden.rs:12-14`) and could `umount` the masks. Filed as a follow-up (see the
"Open verdicts" section).

### 3. Procfs / sysfs re-mount

*Attempt.* Mount a fresh `proc` to reach masked entries (`/proc/1/mem`, `kcore`,
`kallsyms`, `sysrq-trigger`); write `/proc/sys` knobs; reach `/sys/kernel/debug`.

*Result / mechanism.* A fresh `proc`/`sys` mount needs the mount family +
`unshare`, all denied for the runner child (`harden.rs:76-82`). At boot,
`BOOT_SCRIPT` sets `dmesg_restrict`/`kptr_restrict` (`boot.rs:1280-1281`), remounts
`/proc` `hidepid=invisible` with a `hidepid=2` fallback (`:1282-1284`), binds
`/dev/null` over `kcore kallsyms keys timer_list sched_debug sysrq-trigger cmdline
latency_stats modules config.gz kpageflags kpagecgroup` (`:1285-1287`), lays RO
tmpfs over `/sys/kernel/{debug,tracing,security}`, `/sys/fs/bpf`, `/sys/firmware`,
`/proc/acpi` (`:1288-1290`), and remounts `/proc/sys` and `/sys` read-only
(`:1291-1294`). These are best-effort (a failed mask logs one line and boot
continues).

*Verdict.* **Blocked** for the runner child (mount-family + `unshare` denial;
best-effort masks at boot). Actual kernel effect (hidepid honoured, `kcore` empty)
is **KVM-verified**. Shares the PTY caveat of vector 2.

### 4. Execution of unknown binaries

*Attempt.* Drop a new ELF into `/workspace` or `/tmp` and execute it; check whether
anything allowlists or pins it.

*Result / mechanism.* Trusted inputs are pinned by digest at install time —
`crates/colonizer/images.lock` (image, `:1-14`), `vendor/vendor.lock` (microsandbox
/ headscale / tailscale), `vendor/claude-code.lock`, `vendor/node.lock` — and
`PATH` puts the pinned binaries first (`boot.sh`, `export PATH=/opt/node/bin:...`).
Pre-exec validation of the agent binary is ELF-magic only
(`crates/colonizer/src/config.rs:119-131`); there is no per-exec digest check.

*Verdict.* **n/a** — by design. A colony runs arbitrary build tooling from the `rw`
worktree; the containment is the microVM boundary plus the runner's seccomp/cap
profile, **not** a binary allowlist. Executing an unknown binary is only an escape
if a later vector leaves the VM. The pinned supply chain is the regression surface:
a pin move re-runs this checklist.

### 5. Egress reconfiguration from inside

*Attempt.* `iptables`/`nftables`, `resolv.conf` rewrite (given `--accept-dns=false`),
or raw sockets to bypass the gateway / TLS edge; reach another colony.

*Result / mechanism.* The fence is host-side msb network policy, not guest
firewalling: `crates/colonizer/src/sandbox.rs:68-76` passes `--net`,
`--net-default-egress deny` and `--net-rule`; rule compilation order in
`crates/colonizer/src/egress.rs:317-330` places `ALWAYS_BLOCKED`
(`egress.rs:89-112`) after allows so "no configuration can reopen a blocked
destination" (fuzzed `egress.rs:521-600`). The colony reaches the gateway only via
the port-scoped `allow@host:tcp:{port}` (`boot.rs:182-184`, default
`127.0.0.1:41750` at `config.rs:91-99`; tested `boot.rs:1327-1337`), never the broad
`host` profile. Inter-colony traffic is blocked by the Headscale ACL
(`crates/colonizer/src/mesh.rs:299-302`, "VMs cannot reach each other") with
single-use join keys (`mesh.rs:331-346`) and per-colony node deletion
(`lifecycle.rs:68-70`). In-guest, `iptables`/`nftables` need `CAP_NET_ADMIN`
(dropped, `harden.rs:59`); `CAP_NET_RAW` is deliberately kept for package managers
(`harden.rs:5`), but the host policy still decides what leaves the VM, and breaking
`resolv.conf` only breaks the colony's own name resolution.

*Verdict.* **Blocked** at the host edge (msb egress policy is authoritative and
allow-cannot-reopen-block; inter-colony blocked by ACL + single-use keys). Actual
msb enforcement is **KVM-verified**. Retained `CAP_NET_RAW` is a documented
trade-off, not an escape (host policy gates the wire).

### 6. Supervisor / process-injection paths

*Attempt.* `ptrace` / `process_vm_readv|writev` against `agentd` or `tailscaled`;
read `/proc/<pid>/{mem,environ,maps}` of host-reaching processes; signal abuse.

*Result / mechanism.* The runner child is denied `ptrace`,
`process_vm_readv`/`writev`, `pidfd_getfd`, `kcmp` (`harden.rs:83-85`);
`CAP_SYS_PTRACE` is dropped (`:58-65`); `PR_SET_DUMPABLE` is gated (`:151-156`) and
`RLIMIT_CORE` is 0 (`:264-273`). `agentd` self-guards non-dumpable + core-0 at
startup (`crates/colonizer-agentd/src/main.rs:141-144`, `harden.rs:349-353`), and
`/proc` runs `hidepid` (vector 3), so the agent cannot read the daemon's
`mem`/`environ`. `tailscaled`/`headscale` live on the host (a different kernel),
unreachable from the VM.

*Verdict.* **Blocked** for the runner child (ptrace-family denial + non-dumpable +
hidepid; regression via the `harden.rs` probe tests and `agentd --exec-hardened`,
`main.rs:136-139`). **Open** via the unfiltered PTY: a shell from `/v1/pty` runs
unhardened and can ptrace. Same follow-up as vector 2.

### 7. Credential-path abuse

*Attempt.* Send the placeholder where the real credential goes (does the edge
refuse-and-log or forward-and-fail?); forge a per-colony gateway token; connect
directly to `api.anthropic.com` with the placeholder (must 401); DNS-rebind
`host.microsandbox.internal`.

*Result / mechanism.* Secrets are provisioned as msb `--secret env@hosts`; the real
value stays in msb's host process and the guest env holds only a placeholder
(`sandbox.rs:62-67`; assembled `boot.rs:964-1001`, `CLAUDE_API_HOST =
api.anthropic.com` at `app.rs:36`). The gateway token is 244-bit random, `0600`
(`boot.rs:577-579`), compared constant-time (`gateway.rs:632-645`); an unknown
token 401s before anything else (`gateway.rs:1148-1156`) and a valid token alone
does not open an unrouted provider (`gateway.rs:1185-1199`). The colony's inbound
`x-api-key`/`authorization` are never forwarded — `FORWARD_HEADERS`
(`gateway.rs:49`) — and the real credential is inserted host-side
(`credential_header`, `gateway.rs:961-971`, `set_sensitive(true)`); placeholder
creds "must never reach an upstream" is tested at `gateway.rs:2256-2262`. Upstream
path traversal is rejected (`gateway.rs:949-958`).

*Verdict.* **Blocked** at the gateway (harness-asserted). The msb TLS-edge swap, the
direct-to-`api.anthropic.com` 401 and DNS-rebinding of
`host.microsandbox.internal` are properties of the msb edge (not in this repo) and
are **KVM-verified**.

### 8. Publish-time smuggling

*Attempt.* `.git`-shaped payloads (hostile `.git` dir or symlink, hostile
`gitdir:` text), nested repos (submodule-shaped `.git` files/dirs), hooks,
fsmonitor, a non-regular `pr.md`, and FIFOs/sockets/devices in the worktree.

*Result / mechanism.* At publish (`crates/colonizer/src/github.rs::publish`,
`:1920-1946`) the kill-switch is checked first (`:1923`), then `restore_gitfile`
(`:1928`, `:1960-1975`) replaces the worktree `.git` with a regular 0644 file
holding the pre-VM recorded `gitdir:`, and `strip_nested_git` (`:1929`,
`:1978-2002`) removes every non-root `.git`. Host git always runs with
`HOST_GIT_NO_EXEC` (`github.rs:153-162`, applied in `git_plain` `:122-132`):
`core.hooksPath=/dev/null core.fsmonitor=false gc.auto=0 maintenance.auto=false`,
plus `--no-verify` on commit (`:1616`). `pr.md` (in the session dir, not the
worktree) is read through `read_regular_file` with `O_NOFOLLOW|O_NONBLOCK` + fstat
(`github.rs:2011-2027`); a symlink/FIFO/dir/oversized file falls back to the
default title (tested `github.rs:3034-3078`). Placeholders are stripped before
`git add -A` (`path_policy::remove_leftovers`, `github.rs:1560-1568`). Special
files (FIFO/socket/device) planted in the worktree are handled by `git add -A`,
which does not stage non-regular files.

*Verdict.* **Blocked**. Regression tests added for the `.git` rewrite, nested-repo
stripping and the `HOST_GIT_NO_EXEC` contents (see `github.rs` tests); the `pr.md`
and blocked-publish paths were already covered. The worktree special-file case
relies on `git add -A` ignoring non-regular files — spot-checked in the manual
procedure.

## Open verdicts (follow-ups)

- **Vectors 2 & 6 — the unfiltered PTY.** `agentd` filters only the runner child;
  the `/v1/pty` shell is deliberately unfiltered for the human's terminal
  (`harden.rs:12-14`, `docs/architecture.md:558`), but nothing enforces that the
  PTY client *is* the human. A runner that reads `/colonizer/token` and connects to
  `ws://127.0.0.1:7070/v1/pty` gets an unhardened root shell, shedding its own
  seccomp/cap profile. Filed as a follow-up; it is a Layer-3 hardening-completeness
  gap, not a microVM-boundary crossing.

## Manual KVM procedure

The attempts above marked **KVM-verified** need a live microVM and cannot run in
CI. Run them before each release checkpoint and record the result in the sign-off
table.

*Hardware / setup.* A KVM-capable Linux host (bare metal, or a cloud instance with
nested virtualisation), microsandbox pinned to the digest in `vendor/vendor.lock`.
Build the mothership (`cargo build --release -p colonizer-harness`), then launch one
colony on a throwaway repo (the `colony-e2e` driver, or the `colonizer` CLI against
a scratch issue). Attach to the colony PTY and run each block from inside the guest.

| # | From inside the colony | Expected |
|---|---|---|
| 1 | `mount --bind /dev/null /colonizer/token; cat /colonizer/token` (as the runner user, not the PTY) | `mount`: `Operation not permitted` (seccomp) |
| 2 | `umount2`/`umount /run/... mask` | `Operation not permitted` |
| 3 | `mount -t proc proc /tmp/p && cat /tmp/p/1/mem` | `mount` fails; if forced, `kcore`/`kallsyms` empty, `hidepid` hides pid 1 |
| 5 | `curl -sS https://example.com` (not on the egress allowlist) | connection refused / blocked by msb policy |
| 5 | from colony A, `ping`/`curl` colony B's mesh IP | no route / blocked by Headscale ACL |
| 7 | `curl -sS https://api.anthropic.com/v1/messages -H "x-api-key: $PLACEHOLDER" ...` | `401` (placeholder is not a real key) |
| 7 | resolve `host.microsandbox.internal` to an attacker IP, retry the swap | swap fails closed; no real credential emitted |
| 8 | plant a FIFO + a symlink to `/etc/passwd` in the worktree, trigger publish | neither is staged; publish output contains only regular tracked files |

The seccomp-denial rows (1-3) can also be observed without KVM via
`agentd --exec-hardened` and the `harden.rs` probe tests, which spawn a real
hardened child on the test host; the microVM run additionally confirms the mask and
egress *effects*.

## Release sign-off

Re-run the KVM procedure (all eight classes) at each release and on any
sandbox-affecting change (image digest bump, `boot.sh`/`BOOT_SCRIPT` change, mesh
or gateway change, `harden.rs` change), then add a row.

| Release | colonizer rev | Date | Signed-off-by | KVM host | Result | Notes |
|---|---|---|---|---|---|---|
| v0.1.3 | (this PR) | — | _pending_ | — | code-derived only | Verdicts above are read from the implementation with mechanisms cited; the live KVM run has not yet been executed. Vectors 2 & 6 carry an open follow-up (the PTY hardening-shed). |
