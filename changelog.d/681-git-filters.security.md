- **Host git over colony content runs with a clean config and a scrubbed environment, and no
  publishing credential.** Every host-side git command now pins the global and system git configs
  out (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`), resets credential helpers, and runs
  from an environment allowlist that drops tokens and `GIT_*` overrides — so a content filter a
  worktree's `.gitattributes` names has nothing defined to run and no token to inherit. The
  credential helper and token now ride only on an explicit authenticated variant used by `fetch`,
  `push`, `ls-remote` and `clone`, which read no worktree content; the mothership's own auto-rebase
  and the reclaim and catch-up paths get the same hardening, and a host-side rebase stamps its
  commits with a fixed host identity since the global config no longer supplies one. The
  authenticated variant keeps only the host's `url.*.insteadOf`/`pushInsteadOf` rewrites from its
  config, and a git credential prompt nobody can answer now fails a boot at once with a clear
  message instead of being retried for 20 minutes. ([#681])
