**Secrets are redacted from every file the mothership writes from agent or model output.** A
credential quoted in a filed finding, an independent review, a colony's `pr.md`, a chat message,
a colony summary or an activity-log line is now stored, and published to GitHub, as
`[REDACTED:<kind>]`: that covers `finding-body.md` and the issue it files, `review.md` and the PR
comment, the commit subject, `pr-body.md` and the pull request, `chats/<id>.jsonl`, the summaries
in `sessions.json` and `activity.jsonl`. A rotated `events-N.jsonl` written before redaction
existed is redacted when it is read back into the cockpit's diagnosis or a resumed colony's
prompt. Redaction is never silent: the colony's log names what was redacted (`pr.md contained 1
secret (github token), redacted before publishing`), and autopilot holds a colony whose `pr.md`
carried a secret until a person presses Create PR. ([#761])
