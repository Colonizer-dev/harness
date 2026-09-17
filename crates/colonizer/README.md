# colonizer-harness

The mothership of [Colonizer](https://colonizer.dev): it runs coding agents in private KVM microVMs, links
them to your machine over a private mesh, and opens their pull requests. The command it builds is
`colonizer`.

This crate is the harness's Rust source. On its own it is not a working install: the app also needs
microsandbox, the in-VM daemon ([colonizer-agentd](https://crates.io/crates/colonizer-agentd)), the agent
module and the web UI beside it. Install Colonizer with:

```sh
curl -fsSL https://colonizer.dev/install.sh | sh
```

or build it from source as the [install guide](https://colonizer.dev/docs/install) describes. The code,
issues and releases are at [Colonizer-dev/harness](https://github.com/Colonizer-dev/harness). MIT.
