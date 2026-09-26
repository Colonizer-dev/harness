- **In-guest hardening for the agent.** The colony's agent runs as root in its microVM, and three
  layers now bound what that root can do before its first instruction: `boot.sh` locks the guest's
  kernel interfaces down (`dmesg_restrict=1`, `kptr_restrict=2`, `hidepid` on `/proc`, the readable
  `/proc` files masked with `/dev/null`, an empty read-only tmpfs over debugfs, tracefs, BPF,
  firmware and the ACPI/SCSI/ALSA corners, `/proc/sys` and `/sys` remounted read-only); agentd
  makes itself non-dumpable with core dumps off, so the agent can neither see nor read the
  daemon's `/proc` entries; and the runner child — the agent and everything it spawns — is exec'd
  with 21 capabilities dropped (mount, ptrace, `SYS_ADMIN`, BPF, kernel modules out;
  package-manager caps in), core dumps off, `no_new_privs`, and a seccomp denylist that turns
  io_uring, userfaultfd, mount, namespaces, ptrace, kexec and friends into ordinary `EPERM` tool
  failures instead of kills. Landlock path pinning waits for a libkrunfw built with it — measured
  2026-09-25, `landlock_create_ruleset` returns `ENOSYS` on the pinned stack's Linux 6.12.99. See
  the In-guest hardening section of [docs/architecture.md](docs/architecture.md). ([#301])

[#301]: https://github.com/Colonizer-dev/harness/issues/301
