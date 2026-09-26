- **An egress policy on the colony fence.** Every colony now boots behind a configurable two-class
  egress policy (`crates/colonizer/src/egress.rs`): `open`, today's behaviour, and `allowlist`, which
  names no public profile, sets `--net-default-egress deny` and reaches only the hosts on the allow
  list. Both modes compile a non-overridable always-blocked deny set — cloud metadata, private and
  loopback ranges, CGNAT, benchmark and reserved ranges, NAT64, DNS64 and 6to4 prefixes — ahead of
  every configured allow, so no setting can reopen them, with `allow@dns` in front so name
  resolution survives. Sandbox settings gain `egress`, `egress_allow` and `egress_block` (validated
  at save time); an org can fix the mode and extend, never shrink, the lists. Each boot records the
  resolved policy to `<session>/egress.json` and serves it at `GET /api/sessions/{id}/egress`. The
  policy is applied at boot: a change takes a stop plus Resume. See the Egress policy section of
  [docs/sandbox-network.md](docs/sandbox-network.md). ([#303])

[#303]: https://github.com/Colonizer-dev/harness/issues/303
