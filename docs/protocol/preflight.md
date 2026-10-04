# Pre-flight scan

Part of the [Colonizer protocol](../protocol.md).

With `COLONIZER_SCAN` set to `warn` or `block` and `COLONIZER_SCAN_COMMAND` naming a scanner, the
runner scans `/workspace` before the agent sees it. Findings arrive as `log` events. In `block` mode a
non-zero exit ends the colony with `status error` before the first prompt; the microVM is still up, so
the terminal remains reachable.

**This is advisory, not a security boundary.** A repository's own `.claude/settings.json` hooks and
`.mcp.json` servers already run inside colonies by design. The boundary is the microVM, the publish
step's sanitizing, and a human reading the pull request. What a scan protects is the task outcome:
prompt injection steering the agent into work nobody asked for.

It runs inside the colony and never on the mothership: repository content is attacker-controlled, and
the mothership holds every credential. A scanner that cannot start, or that runs past its 120-second timeout, is
reported and treated as no findings: a broken scanner must not be able to halt every colony.
