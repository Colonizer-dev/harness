# Map refresh

Part of the [Colonizer loops](../loops.md).

A map loop keeps architecture maps fresh instead of running a prompt: each firing launches the same
mapping colony as the Map view — same instructions, same `archify` skillset, reuse of a colony
already drawing — so its runs are the map, exactly as if it had been drawn by hand. Its colonies
carry the origin `map:loop:<id>`. Its prompt is not used and may be left empty. Choose a scope:

- **This repository** — maps the one repository on its cadence.
- **All repositories in the org** (`owner/*`) — maps them one at a time, ten minutes apart, and
  re-lists the org at the start of every cycle, so repositories added later are included. The next
  repository starts even while the previous one is still drawing.

Launches go through the ordinary admission path, so parallel limits, org budgets and the archify
skillset rule the loop in exactly as they rule the Map view. A firing that is refused says so in the
loop's note and tries again at its next slot; org-wide, one repository's failure is noted and the
cycle moves on to the next — and a workspace that is switched off altogether skips its whole cycle
with that single note rather than one per repository. When a refresh ends, History records it as
`map.refresh` — and a repository whose refresh failed keeps its old map.

**Keep this map up to date?** — the Map view asks this once a repository has a map and no enabled
map loop covers it. The answer defaults to every 14 days, with presets 7/14/30/60/90 or a custom
number of days (up to 365), for this repository or all repositories in its org; the loop runs at
03:00 your local time. **Not now** is remembered in that browser for 30 days, and the map bar's
**Keep fresh…** link asks again. Once a refresh loop covers the repository, the map shows
"Refreshed every N days · edit". Map loops can also be made from the Loops page.
