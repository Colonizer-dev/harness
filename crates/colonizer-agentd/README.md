# colonizer-agentd

The daemon inside every [Colonizer](https://colonizer.dev) microVM: it runs the agent, keeps the event log
and serves the colony's terminals, for the mothership
([colonizer-harness](https://crates.io/crates/colonizer-harness)) to reach over the mesh. Colonizer builds
it as a static musl binary and mounts it into each colony, so there is nothing to install by hand.

Install Colonizer with `curl -fsSL https://colonizer.dev/install.sh | sh`, or see the
[install guide](https://colonizer.dev/docs/install). The code, issues and releases are at
[Colonizer-dev/harness](https://github.com/Colonizer-dev/harness). MIT.
