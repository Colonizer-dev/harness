- **A shared anti-spam ledger for the mothership's proactive messages.** Every outbound proactive
  action — a notify announcement, an autonomous judge answer — is now counted in one ledger
  (`<data_dir>/ledger.json`) before it leaves: duplicate facts within a window are dropped, a
  question that blocks its colony bypasses the soft layers but never the hard ones (quiet hours are
  configured, not yet on by default; a per-topic daily cap and per-kind hourly and daily quotas
  always are), and what the soft layers hold is summarised once an hour as one line ("Colonizer: 5
  held announcements — provider_degraded ×2, question ×3") with counts by class only, no colony ids
  or question text. The tallies ride the authenticated `/api/status` as `ledger`. The watchdog's
  nudges join the ledger in a later slice. ([#311])

[#311]: https://github.com/Colonizer-dev/harness/issues/311
