- **Colony agents can no longer ptrace, unshare or mount**, and `/proc/sys` and `/sys` are
  read-only in the guest: the runner child's seccomp denylist answers those with `EPERM` and the
  boot script remounts the kernel filesystems before the agent runs. A workload that relied on one
  of them now fails with an ordinary tool error instead of succeeding. Check a workload against
  the profile with `colonizer-agentd --seccomp-profile` and
  `scripts/seccomp-evidence.sh -- <workload>`. ([#301])

[#301]: https://github.com/Colonizer-dev/harness/issues/301
