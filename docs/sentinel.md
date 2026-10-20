# Sentinel shadow-mode pre-execution verdict (issue #1323)

A narrow detection system for risky tool calls that might bypass existing safety controls. The sentinel runs in shadow mode only: verdicts are logged to the events ledger but never enforced. The hard boundary remains with `exec_policy` and `path_policy`.

## Risky classes

- **mount/umount/chroot/nsenter/unshare/pivot_root**: privilege escalation attempts in shell commands
- **Writes under protected paths**: `.git/config`, `.git/hooks`, `.claude/`, `.codex/`, etc.
- **File-tool writes to protected directories**: `Write`, `Edit`, `NotebookEdit` targeting protected paths
- **Credential-adjacent paths**: secrets files (read-only operations are allowed)
- **curl/wget to non-localhost destinations**: classified as ask (p≈0.5) since egress policy is not accessible in shadow mode
- **kill/pkill/killall of harness processes**: pid 1, sentinel process ids, agent runner

## Verdicts (shadow-only, never enforced)

- **allow** (p≈0.8): read-only commands on credential paths
- **refuse** (p 0.92-0.98): high-confidence risky operations  
- **ask** (p≈0.5): egress operations (curl/wget to non-localhost)

## Operation

1. classify() examines tool name and input for risky patterns
2. verdict() returns confidence (p), reason, and stable hex digest
3. Digest binding: SHA256 of (tool, policy_version, colony_id, input_json)
   - Includes cwd only if present in the tool_call event
   - Stable across retries and session restarts for the same tool call
4. Pending map: bounded at 10k per colony; insertion refused when full
5. Ledger: decision rows written via `decide::record()` with:
   - `kind: "decision"`, `point: "sentinel"`, `mode: Shadow`
   - `pick`: "allow" (read-only credential paths), "ask" (egress), "refuse" (others)
   - `outcome`: JSON with risk, outcome (ran/error/refused_by_policy), reason, digest, cmd (truncated to 200 chars)

## False alarms

Read-only commands touching credential paths are explicitly allowed (p≈0.8) to prevent false holds from issue 1079.

## What remains

- **Enforcement**: verdicts advisory; hard boundaries are exec_policy and path_policy
- **Egress integration**: future work to check actual egress policy against curl/wget targets if accessible
- **Defeat hook**: potential integration with defeat.rs for false-alarm feedback via `likely_false_alarm` marker
