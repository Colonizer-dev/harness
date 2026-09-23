# Gaps: design ahead of code

[vision.md](vision.md) and the illustrations on [colonizer.dev](https://colonizer.dev) are the design.
The cockpit and the settler cards were also ported from two design prototypes that are not in this
repository: the cockpit prototype, "Colonizer Cockpit", first ported in
[#187](https://github.com/Colonizer-dev/harness/pull/187) and caught up with in
[#194](https://github.com/Colonizer-dev/harness/pull/194), and the Settler Showcase
([#71](https://github.com/Colonizer-dev/harness/pull/71)). This page lists the elements they depict
that the code does not implement yet, as far as they have been checked, so that something remembered
from a picture can be checked here before it is reported as a regression. Nest text balloons were
once reported as lost when the nest had never drawn them
([#232](https://github.com/Colonizer-dev/harness/issues/232)).

It is a register, not a roadmap. Where the code does not yet meet the security and trust claims,
[audit.md](audit.md) says so; planned work is the horizon in [vision.md](vision.md) and the issues.

## Not built

| Element | Where the design shows it | What the code has instead | Issue |
| --- | --- | --- | --- |
| Role captions under a crew's ants | `colonizer-website/colonies.html`, "a crew on one trail" illustration | The ants on one trail and a count ("3 settlers · all done"); each ant's name is only its hover title (`web/src/components/ChatPanel.tsx`, `CrewStrip`) | none filed |
| Previews over the mesh | `docs/vision.md`, principle 5: "a hop away for chat, terminals and previews" | Chat and terminal only (`web/src/components/ChatPanel.tsx`, `web/src/components/TerminalPanel.tsx`); previews are on the same page's horizon and `PLANNED` in `README.md` | none filed |
| A settler count on each overview row | Cockpit prototype, overview ([#194](https://github.com/Colonizer-dev/harness/pull/194)) | No count: the mothership streams one colony's events at a time (`web/src/cockpit/OverviewView.tsx`) | none filed |
| Per-file +/- counts on the pull request card | Cockpit prototype, inspector ([#194](https://github.com/Colonizer-dev/harness/pull/194)) | Pull request number, publish stage and branch; the API reports no diff stats (`web/src/cockpit/Inspector.tsx`) | none filed |
| Inbox and history read from an event log | Cockpit prototype, inbox and history ([#187](https://github.com/Colonizer-dev/harness/pull/187)) | One entry per colony: its current state, stamped with its `updated_at` (`web/src/cockpit/feed.ts`) | none filed |
| Today's spend in the header | Cockpit prototype, header ([#187](https://github.com/Colonizer-dev/harness/pull/187)) | The workspace's running total, "$N spent" (`web/src/cockpit/Header.tsx`) | none filed |
| A done settler's duration, and "retried" after a failed step | Settler Showcase, settler card ([#71](https://github.com/Colonizer-dev/harness/pull/71)) | The step count, and "didn't work" on a failed step; the stream records neither (`web/src/components/SettlerCard.tsx`) | none filed |

## Checked and built

Elements that are easy to remember as missing, and where they are.

| Element | Where the design shows it | Where the code draws it |
| --- | --- | --- |
| What each settler is doing, in words | `colonizer-website/index.html`, "01 / the colony" (window "127.0.0.1:7878 · colonizer"): a card per settler with name, task, status, summary, step count and "Show work" | The colony window, not the nest: `web/src/components/SettlerCard.tsx`; the summary shows once a settler is done and its card is closed |
| Text balloons in the nest | Nowhere on the website: it never draws the nest | Since [#269](https://github.com/Colonizer-dev/harness/pull/269) for [#218](https://github.com/Colonizer-dev/harness/issues/218), a balloon saying what the colony is doing on each chamber with room for one; the selected chamber always has one (`web/src/cockpit/NestView.tsx`, `planBalloons`, decides which). Per settler: each ant's hover title, and the inspector's SETTLERS list (`web/src/cockpit/Inspector.tsx`). A bubble per ant is asked for in [#397](https://github.com/Colonizer-dev/harness/issues/397) |
| "Described in plain language" and "Show detail" | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/ChatPanel.tsx` |
| The crew strip, "3 settlers · all done" | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/ChatPanel.tsx`, `CrewStrip` |
| A question card with choices and "Other…" | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/AskUserCard.tsx` |
| Create PR, Stop, Clean up, and the activity strip | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/SessionView.tsx` |
| The sidebar: orgs, connections, colonies, memory | `colonizer-website/index.html`, "01 / the colony" | `web/src/components/Sidebar.tsx`, in windows narrower than 900px; a wider window shows the cockpit's rail instead (`web/src/App.tsx`, `web/src/cockpit/Rail.tsx`) |
| Twelve settler roles | `colonizer-website/colonies.html`, "03 / settlers" | `web/src/settlers.ts`, `web/src/components/AntAvatar.tsx` |
| Five ant states, and a stumble on a failed step | `colonizer-website/colonies.html`, "the ant shows what its settler is doing" | `web/src/components/AntAvatar.tsx`; "stopped" is its `paused` state |
| A model for each kind of work | `colonizer-website/index.html`, "03 / the router" illustration | `web/src/components/SettingsDialog.tsx` |

## Keeping it true

A pull request that builds a **Not built** element moves its row to **Checked and built**, or drops
it. One that adds or changes a claim in [vision.md](vision.md), or ports a design and leaves part of
it out, adds a row. The pull request template asks.

`scripts/test/gaps.test.mjs` checks that every repository path cited here exists, and that every
**Not built** row names a tracking issue or says `none filed`. It cannot check the pictures: the
website lives in another repository, so when an illustration changes there, record it here by hand.
