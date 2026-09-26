- **Prompt screening at publish time.** A new opt-in `screen` module (provider `promptdecode`)
  scans the colony's final diff and the pull request body for hidden code points — tag characters
  encoding ASCII, bidi controls reordering what a reviewer reads, variation selectors smuggling
  bytes — after the commit and before the push, the first moment anything could leave the machine.
  A tiny deterministic decoder built into the mothership: no heuristics, no network, no model.
  `warn` (the default once the module is on) logs the findings, records them on the colony's event
  log and lists them in a sanitized section at the foot of the pull request;
  `block` holds the publish — no push, no pull request — naming the count per class and the way
  out. Flag emoji, ZWJ sequences, and Arabic or Hebrew prose without direction controls pass
  clean; direction embeddings, isolates and overrides are flagged even when balanced. Off until
  configured, like notify. See [docs/prompt-screening.md](docs/prompt-screening.md).
