# The cockpit

The cockpit is Colonizer's web UI. The mothership serves it on the same address as its API,
`http://127.0.0.1:7878` by default. This page walks through each view: what it shows, how to reach
it, and where it stops. For the HTTP routes behind the views, see [protocol.md](protocol.md).

## Signing in

Every browser signs in once with the install's API token. When the mothership starts it prints a
sign-in link and opens it if there is a browser:

```text
cockpit: http://127.0.0.1:7878/?token=<token>
```

To get the link again, run this on the machine where the mothership runs:

```sh
colonizer open
```

It prints the link and opens it in the default browser (`open` on macOS, `xdg-open` on Linux with a
display). Set `COLONIZER_NO_BROWSER` to skip the browser and only print it.

Following the link sets a `colonizer_token` cookie (HttpOnly, SameSite=Strict, kept for about a
year) and then removes `?token=` from the address bar, so the token does not stay in your history.
A browser without the cookie gets a "Sign in to your cockpit" page that tells you to run
`colonizer open`.

Limits:

- `colonizer open` works only on the mothership's own machine. It reads the local token file
  (`<config_dir>/api-token`) and ignores `--host` and `--token-file`.
- When the mothership is bound to `0.0.0.0` or `[::]`, the link points at loopback (`127.0.0.1` or
  `[::1]`). To reach the cockpit from another device, use [remote access](#remote-access) or your
  own TLS proxy or SSH tunnel.

Add `?mock=1` to the address to run the cockpit against a built-in fake backend. It needs no
mothership and is useful for trying the UI.

### Hosted demo

The same mock also ships as a static bundle: `npm run build:demo` in `web/` writes `web/dist-demo/`,
a cockpit build with the mock forced on (no `?mock=1` needed). It makes no `/api` calls, registers
no service worker, and carries no install manifest, so it is a plain page. The website repo serves
it at `https://colonizer.dev/demo`; the host needs one SPA rule — every `/demo/*` path that is not a
file in the bundle serves `/demo/index.html` with a 200 — because the cockpit keeps its view in the
page rather than in the address. The demo is not published yet, so do not expect the link to work
until the website repo ships it.

## Layout

The layout depends on the window width:

| Width | What you get |
| --- | --- |
| 900px and wider | The full cockpit: sidebar on the left, top bar, the current view. |
| 640px to 899px | The older layout: a colony list in a slide-out drawer and the open colony. The cockpit views are not shown at this width. |
| Under 640px | The cockpit with a tab bar at the bottom instead of the sidebar. See [Phones and narrow screens](#phones-and-narrow-screens). |

### Sidebar

From top to bottom, the sidebar has:

- the workspace switcher ([Workspaces](#workspaces-and-the-switcher)),
- the **Colonize** button ([Colonize](#colonize)),
- Overview, Nest, Chat, Code, History, Loops, Memory, Host and Secrets,
- an **Update** row when a newer version is out (it opens Settings, Updates),
- Settings, and a light/dark theme toggle.

Nest shows how many colonies are live. Memory shows how many memory proposals wait for review.

Press **⌘B** (Ctrl+B on Linux and Windows) to collapse the sidebar to icons, or expand it again. The
shortcut does nothing while you are typing in a text field. The cockpit remembers the choice.

The Inbox has no sidebar item. It is behind the bell in the top bar.

### Top bar

The top bar shows the avatars of the workspaces that have colonies running now. Click one to filter
to that workspace. At the far right is the notifications bell (see [Inbox](#inbox-and-the-bell)).
The bar also warns when the mothership stops answering or the live feed drops. While remote access
is on, it shows a "Remote access on" badge that opens its settings.

### Page width

Dashboards, tables and lists fill the window up to a generous cap, so they stay readable on very
wide screens. Forms (Secrets, the Launch picker, Settings) are capped at a narrower reading width.
Chat, the code editor, an open colony and Memory use the whole window. The Nest is a full-width
canvas.

## Workspaces and the switcher

A workspace is a GitHub organization (or your own account) that Colonizer works in. The switcher at
the top of the sidebar sets the scope for every view: **All workspaces**, or one workspace.

- Each row shows live, queued and total colonies, and how many need you.
- With more than six workspaces, the menu has a search box. Type to filter, press Enter to pick
  the first match.
- The arrow keys, Home and End move through the menu. Escape closes it.
- The last row opens the chosen workspace's settings ("Manage workspaces" when none is chosen).
- Workspaces you switched off are listed at the bottom. They cannot be picked, but clicking one
  opens its settings so you can switch it back on.

When Colonizer sees a new organization for the first time, a card at the top of the page asks
whether to add it as a workspace.

## Overview

**Sidebar: Overview**, or click the "colonizer" name at the top of the expanded sidebar.

With **All workspaces** chosen, Overview shows everything at once:

- a summary line: colonies that need you, live and queued colonies, workspaces, and spend,
- KPI tiles,
- **Needs you**: colonies waiting on a person, oldest first. "Answer" opens the colony; you answer
  there, not on this page,
- **Merged PRs per day**, stacked by workspace, next to each workspace's share,
- **Workspaces**: one row per workspace. Click a row to open its dashboard. Each row also has the
  red-team buttons ([Red team](#red-team)),
- **Colonies**: every colony in a table,
- a system strip about the host,
- the [storage panel](#storage).

The range picker switches between 7, 30 and 90 days. **Compare** overlays the previous period of
the same length.

The Colonies table pages ten rows at a time. Each column title is also its filter: search by colony
name, pick workspaces, pick statuses, limit "Updated" to the last 1h, 24h or 7d, or "Spent" to more
than $1 or $5. "clear filters" resets them.

Limits: change failure rate is derived from current colony statuses (failed ÷ merged + failed in
the window), because there is no failure history. Merged PRs are bucketed by merge date, or by
creation date when the mothership does not send one. The page says so under each figure.

### Workspace dashboard (per org)

Pick a workspace in the switcher, or click its row in the Workspaces table. Overview then shows
that workspace's dashboard. "← All workspaces" goes back.

The dashboard has repository tabs, KPI tiles, colony outcomes next to the delivery funnel, spend by
model next to the token mix, a Repositories table and the workspace's colonies. Lead time, PR cycle
time and CI pass rate are computed from the colonies' pull requests. Figures with no data source
(coverage, time to recover, per-day latency, a lead-time histogram) are shown as "unmeasured"
rather than guessed.

The Repositories section has a **Repositories | Packages** switch. See [Packages](#packages).

## Nest

**Sidebar: Nest.** This is the default view.

The Nest draws every colony in the current scope as a chamber dug under the mothership. A tunnel
joins each chamber to the surface. It is thick and moving while the colony works, and dashed once it
stops. A strip of live events runs across the page.

- Click a chamber to select that colony. The chamber zooms open to show the colony's ants at work,
  and the inspector opens on the right with its status, questions and Stop / Resume buttons.
- Double-click a chamber to open the colony itself (its chat, terminal and publish controls).
- Click the mothership to see host-wide figures in the inspector.
- The dashed **DIG** chamber opens the Launch view.
- During a [red-team run](#red-team), red ants gather over the nest.

Limit: the mothership streams events for one colony at a time. Only the selected colony shows its
real subagents. Every other chamber shows one ant whose state comes from the colony's status.

### Workspace panel

While no colony is selected, a panel on the right sums up the scope: colony states, what needs you,
cost and recent activity. It is open by default on windows 1280px and wider. Hide it with its close
button, and bring it back with the **Dashboard** button at the top right of the Nest. Selecting a
colony swaps it for the inspector.

### Nest map

The **Nest | Map** switch at the top of the Nest shows the same colonies walking a map of one
repository's architecture.

- Pick the repository in the bar above the map.
- With no map yet, **Map this repo** starts a mapping colony that reads the code and draws the map.
  "Watch it work" opens that colony. **Redraw map** starts a new one later.
- Components are chambers, boundaries are mounds, and connections are tunnels. Each live colony's
  ants walk to the components whose files it is changing.
- Click a component to see its files in an explorer pane, with the files live colonies are changing
  flagged. Click a file to see which colonies are on it, what they did there and their diff.
- **Raw JSON** shows the stored map.
- Zoom with the + and − buttons, a pinch, or ⌘/Ctrl + scroll. Drag to pan. **Fit** returns to the
  whole map. A plain vertical scroll scrolls the page, not the map.

The first time you open a map, the cockpit offers to create a loop that keeps it fresh. "Not now"
stays quiet for a month. **Keep fresh…** brings the offer back. See
[loops.md](loops.md#map-refresh).

## Colonize

**Sidebar: Colonize** (the orange button), the Colonize button on the Overview and dashboard title
row, or **⌘K** (Ctrl+K on Linux and Windows) from any view.

⌘K opens Colonize everywhere in the cockpit, except inside the code editor or a terminal, which keep
the key for themselves. It no longer opens the floating composer.

The pane has two parts.

**The box at the top.** Type or dictate a task and press Enter. The mothership drafts it into one
GitHub issue, or up to five when the text clearly lists separate tasks. You confirm or edit the
drafts, then **Create** files them on GitHub. New issues appear at the top of the list, selected, and
are dispatched at once unless you turn off "dispatch right after creating".

- Drafting uses the cheap summary model. With no summary model configured, your text becomes a
  single draft as written.
- New issues go to the repository chosen in the pane. If the scope has several and none is chosen,
  the confirm step asks which one.
- `/loop 1h <task>` creates a loop instead (see [loops.md](loops.md)).
- **Launch without an issue** starts a colony on the text directly.
- The **launch form** link opens the full Launch view.

**The issue list.** The open issues of the repositories in scope, after the Source module's label
filter. Search by title or `#number`, filter by label, page ten at a time. Tick issues and press
**Dispatch** to start one colony per issue. Issues already held by a live colony, and epics, cannot
be ticked. With "all repositories" chosen, the pane loads the 30 most recently pushed repositories
that have open issues.

Escape closes the pane, or steps back from the confirm step.

Limits:

- The button's count is GitHub's open-issue count (which includes pull requests) until the pane has
  loaded the filtered list.
- Colonize is disabled until GitHub is connected.
- If the Source module has include labels, a new issue does not get them. The pane keeps it in its
  own list so you can dispatch it, but it will not show in the filtered list after a reload.

### The floating composer

On Overview, Nest, Inbox, History, Memory and Host, a composer pill floats at the bottom. Click it
to open it. It has two modes: **Launch colony** starts a colony on the chosen repository (type `#123`
or pick a suggested issue to link one), and **Ask** sends the text to Chat. It also accepts
`/loop <interval> <task>` and dictation.

## Launch

The Launch view is the full launcher: pick a repository, select one or many issues, and launch. It
checks for duplicates and queues past the parallel limit. Reach it from Colonize's **launch form**
link, the Nest's **DIG** chamber, or **More → Launch** on a phone.

## Chat

**Sidebar: Chat.**

Chat talks to a model directly. There is no colony and no microVM. Conversations are stored on the
mothership, and replies stream in as the model writes them.

Which models you can use: any configured `<provider>/<model>` from Settings, Model providers, or a
Claude model when there is an Anthropic API key or an Anthropic provider. Chat never uses the Claude
subscription login; that is for colonies only. The model chip shows each provider, whether its key is
set, and its price per million tokens.

**Conversation list.** Search, a workspace filter, pinned conversations first, then Today, Yesterday,
Previous 7 days and Older. Rename (F2), pin, or delete (Delete, or ⌘/Ctrl+Backspace) with an undo.

**Attachments.** The **+** menu attaches a repository file, a colony (its summary and recent
activity), an architecture map or one of its components, a GitHub issue, a text snippet, an image,
or a digest of today's colonies or this week's merged pull requests. Images can also be pasted or
dropped in when the model can see images. The composer estimates what the attached context costs.

**Slash commands.** Type `/` for `/colony`, `/file`, `/loop` (turn the conversation into a
scheduled loop), `/model`, `/system` (edit the system prompt) and `/clear` (new conversation).

**Compare.** Press **Compare** in the composer and pick a second model. Each message then goes to
both models and the replies show side by side.

**Per-message actions.** Copy; edit and resend; regenerate, or regenerate with another model;
**Branch from here**, which copies the conversation up to that message into a new one and leaves the
original unchanged; **Turn into a colony**, **Create a loop** or **Create a GitHub issue** (each
opens a dialog you confirm); and a thumbs-down that keeps a private note on the mothership.
Inline code that names a repository file opens it on the [Code page](#code).

**Personas.** The picker in the top bar sets the system prompt from four presets, each drawn as an
ant:

| Ant | Persona | What it does |
| --- | --- | --- |
| Pip, the forager | Plain | No system prompt. |
| Sarge, the soldier ant | Code reviewer | Bugs, security problems, missing tests, with a fix for each. |
| Silka, the weaver ant | Architect | How the parts fit, trade-offs, the simplest design. |
| Mellie, the honeypot ant | Release writer | Release notes grouped by Added, Changed, Fixed, Security. |

Each card can show the full prompt. Edits to a preset's prompt are saved on the mothership.

**Export.** The download button in the top bar saves the conversation as Markdown, or as a zip with
the images beside it when the conversation has images.

**Keys.** ⌘\ toggles the conversation list. ⌘⇧O starts a new conversation. ⌘F searches the current
conversation (Enter and Shift+Enter move between hits). Escape stops a reply that is streaming. The
send key is Enter by default; the button beside Send switches it to ⌘/Ctrl+Enter. On Linux and
Windows, read Ctrl for ⌘.

## Code

**Sidebar: Code.**

The Code page shows one workspace's repositories as code. It needs a workspace: with **All
workspaces** chosen, it asks you to pick one.

The header shows the number of repositories, lines of code, commits in the last 52 weeks and a
language bar. Below it, **Grid | List** switches between repository cards and one compact row per
repository. Figures come from the mothership's local clones and from GitHub through `gh`.

Click a repository to open the editor:

- file explorer, tabs and a Monaco editor,
- a branch switcher,
- per-file history with compare, and blame,
- "Ask AI" about the open file,
- ⌘⇧F (Ctrl+Shift+F) for full screen; Escape leaves it.

Edits are autosaved as drafts on the mothership, not on GitHub. Autosave is on by default and can be
turned off in the editor. With it off, the editor warns before you close a file or leave the page
with unsaved edits. Drafts are restored when you reopen the branch, with a warning if the file
changed upstream. Nothing reaches GitHub until you press **Create PR** and confirm.

## History

**Sidebar: History.**

History is one timeline of what colonies did and what people did in the cockpit, newest first and
grouped by day. It reads the mothership's activity log (see
[protocol.md](protocol.md#69-activity-log)). Runs that ended with nothing to change fold into one
row.

- Stat tiles at the top (pull requests opened, merged, failed, questions, your actions) act as
  filters.
- Filter by kind (Everything, Outcomes, Questions, Launches, Your actions, Failures), by repository
  and by actor, or search.
- Click a row to open its colony, or the view or settings section it refers to.
- The list shows 50 rows per page. **Load older** fetches older entries from the log.

Limit: outcomes from before the activity log existed come from the colony list and are marked as
approximate.

## Inbox and the bell

The bell at the top right counts the colonies waiting on a person, across all workspaces. Click it
for a short list: who needs you, then what the colonies have said. Each line opens its colony.
**Open inbox** opens the full Inbox view. On a phone, Inbox has its own tab.

The Inbox shows where to go, not the question itself. The question and its choices live in the
colony's chat, so you answer there.

How you get told when the tab is not in front is set in Settings, Notifications
([Notifications and Web Push](#notifications-and-web-push)).

## Packages

**Overview → a workspace → Repositories | Packages → Packages.**

The Packages tab lists, for the workspace's repositories:

- **Published**: what each repository publishes (npm, crates.io, PyPI, Go, pub.dev, SwiftPM, GitHub
  Packages), with a warning when the repository's version is ahead of the registry.
- **Dependencies**: what they depend on, read from their lockfiles.
- **Supply chain**: risks in those dependencies, such as known vulnerabilities, yanked or deprecated packages,
  install scripts, very new releases or packages, low downloads, possible typosquats, licence
  problems, unpinned sources or versions, wildcard ranges and missing integrity hashes. Filter by
  severity, kind and whether a fix exists. Each risk has a button that starts a colony, on autopilot,
  to fix it.

Lists page ten at a time with search and filters.

The mothership reads its own clones and the public registries. The first scan of a workspace runs in
the background and the tab shows "scanning" until it finishes. After that it answers from a cache
kept on disk, shows when it was updated, and refreshes in the background. **Refresh** asks for a new
scan.

## Loops

**Sidebar: Loops.**

Loops are saved prompts that launch a colony on a schedule: every N minutes, daily, every N days,
weekly, monthly, or self-paced (each run sets the next). The page lists each loop with its next run
and how the last one went. **New loop** starts from a template or from scratch. A loop's history
lists every colony it launched. **Run now** starts one immediately. Deleting a loop keeps its past
colonies.

Details are in [loops.md](loops.md).

## Red team

**Overview → Workspaces table → the red-team button on a workspace's row.** The second button opens
that workspace's red-team history and schedules.

The wizard has three steps:

1. **Who hunts and where.** Pick the hunter and the repositories.
2. **Models and swarm size.** Pick the models and 1 to 8 hunters per repository. Each hunter is a
   full colony in its own microVM.
3. **Review.** See the cost estimate and choose **Once, now**, **Weekly** or **Monthly**.

A one-off run waits until no colony is live, then starts. A schedule is kept by the mothership.

Only **Colony swarm** runs today. **Strix** and **Shannon** appear in the wizard but cannot be
picked:

- Shannon always shows "Coming soon".
- Strix shows "Installed · runs coming soon" once it is installed, and "Coming soon" otherwise.

Red-team runs do not drive their scans yet. See [red-team.md](red-team.md) for how runs work and
[security-hunters.md](security-hunters.md) for the hunter modules.

## Memory

**Sidebar: Memory.**

Shared memory is notes that colonies can search while they work, at global, workspace and
repository scope. Colonies propose new notes; nothing is shared until you approve it. The view lists
pending proposals grouped by colony, and the approved notes, which you can edit or delete. The
sidebar badge counts proposals waiting for review.

## Host

**Sidebar: Host.**

The machine the colonies boot on: resources (and a warning if KVM is unavailable), the colonies on
this host, storage, the runtime, and the fleet of peer machines when there are any. Trend lines
cover only the readings taken since you opened the page; the mothership keeps no host history.

## Secrets

**Sidebar: Secrets**, or the account menu in the top bar.

Every key the mothership holds and where it lives: the system keychain or a file readable only by
you. Values are never shown. Each row can set or replace its value, remove it, or move it between
the keychain and the file. Nothing moves on its own. A model picker's **Set key** link lands on the
matching row.

## Storage

**Overview (All workspaces) → the storage panel at the bottom.**

The storage panel shows data-directory usage by category and free space against the warning and
floor thresholds. It lists colonies whose files can be reclaimed, each with a **Clean up** button.
The microsandbox home is shown for information only: it holds the shared image cache and is never
offered for cleanup. Below that is the log archive: its size, and a retention form that previews
what would be removed before you apply it.

## Settings

**Sidebar: Settings** (at the bottom). Settings opens as a full page. Its sections:

- **General**: Setup, Connections (GitHub and Claude), Model providers, Runtime, Live map, Remote
  access, API tokens, Fleet, Updates ([updates.md](updates.md)), Usage data
  ([usage-data.md](usage-data.md)), Notifications, Desktop.
- **Modules**: one page per module, such as the agent, the source of issues, and memory.
- **Workspaces**: one page per workspace.

### Remote access

**Settings → Remote access.** Off by default.

Turn on **Allow remote access** to open this cockpit from a phone or another computer. The
mothership then dials out an encrypted tunnel to the Colonizer relay (`wss://my.colonizer.dev`, or
`COLONIZER_REMOTE_URL`) and gets a link of its own. Nothing listens on a public port on your machine.
Turning it off closes the tunnel and drops every request in flight.

- **Your link** appears once the relay has answered. Copy it, or show a QR code for a phone camera.
- The link asks for a GitHub sign-in. The first sign-in shows a six-digit code that expires after
  10 minutes. It appears under **Pairing** with the GitHub account that asked. **Confirm** it if
  your phone shows the same code; after that, only that GitHub account can sign in. **Reject** it
  if you did not just sign in. The cockpit behind the link still asks for its own token, as for
  any new browser.
- Once paired, **Pairing** names the owner. **Unbind** (asked twice) removes it: the owner's
  sign-in stops working at once, and the next sign-in shows a new code.
- Confirm, Reject and Unbind work only in the cockpit on this machine. Through the link they are
  refused, so nobody who reaches the link can pair themselves.
- **Reset link** makes a new identity and link. The old link stops working, and its owner is
  unbound. Use it if a link leaks.

The link exposes this cockpit, including colony terminals, and nothing else on the machine. See
[remote-tunnel.md](remote-tunnel.md) for the tunnel contract,
[protocol.md](protocol.md#610-remote-access-tunnel) for the routes, and
[remote-access-review.md](remote-access-review.md) for the security review.

### API tokens

**Settings → API tokens.**

A scoped token is a named key for a CLI, an agent or a CI job, so the per-install owner token stays
where it belongs. The pane lists every token: its name and scope, its org and repo limits ("all
repositories" when there are none), its launch caps ("no caps" when uncapped), the day it was made,
and when it was last used — a stamp that moves at most once a minute.

**Create a token** takes a name, a scope (`read` watches, `operate` also drives colonies that
exist, `launch` also starts them), optional org and repo limits, and optional caps: the most
concurrent colonies and a model-spend budget per UTC day. The new token's plaintext is shown once
with a Copy button, and nothing can read it back afterwards — copy it before pressing **Done**.

**Revoke** asks for confirmation, then the token stops authenticating at once. The same three verbs are
`colonizer token create|list|revoke`; what each scope may call is in
[cli.md](cli.md#scoped-api-tokens).

### Fleet

**Settings → Fleet.** Where motherships join each other ([fleet.md](fleet.md)): this machine is
either a fleet's owner, a member of another's fleet, or in neither.

As the owner, **Create invite** mints a single-use code, shown once, that lives 15 minutes — hand it
to the joining machine's operator together with this cockpit's URL. When that machine redeems it, a
pending request appears showing its own six-digit code: **Approve** only if the joining machine's
screen shows the same code, else **Reject**. **Remove** (asked twice) ends a membership.

As a machine joining, enter the owner's URL and the invite code, and both screens then show a
six-digit confirmation code. **Codes match** finishes the join once the owner has approved; until
then it says to wait and can be pressed again, and **Cancel** abandons the join. A member sees the
owner's URL and **Leave fleet** (asked twice). Membership hands the other side a `fleet`-scoped
token only — never this cockpit's own credential.

### Notifications and Web Push

**Settings → Notifications.** These switches are kept in the browser, per browser:

- **In this tab**: the number of colonies that need you in the tab title, a dot on the favicon, and a
  strip above the colony list. The same count is the app badge on the installed app's icon, set
  whenever the session list moves and cleared again at zero; a browser without the Badging API simply
  has no badge, and nothing else changes.
- **Play a sound when a colony asks a question.**
- **Browser notifications while the tab is not in front.** Switching it on asks the browser for
  permission. If the browser blocks notifications for the site, allow them in the browser's own site
  settings first.

"Needs you" counts colonies with an open question (one answered while the colony was suspended no
longer waits on you — it is queued for a slot), live colonies the watchdog has flagged (stalled or
out of nudges; a model error only once the turn has stopped), and failed colonies nobody has opened
yet. Answering a question clears its colony everywhere at once: the mothership pushes a silent
"resolved" note to every enrolled device (while the notify module is on; with it off nothing was
announced), each of which closes that colony's notification and lowers its badge. The same happens when you open a failed colony nobody has looked at yet. Nothing else
closes a notification: the cockpit reports a colony seen only while the page is in front, and only
for that unseen failure, so a colony whose question is still open keeps counting — and keeps its
notification on other devices — until its question is answered.

**Web Push** reaches a device even when Colonizer is closed. Press **Subscribe** under "Push to this
device" to enrol the current browser. Each enrolled device is listed with its name (rename it in
place), when it was last seen, **Send test**, **Prefs** and **Revoke**. Tapping a notification opens
the colony it names. Notifications are grouped one per colony — a colony's next push replaces its
last instead of stacking up — and when two or more colonies need you, a "N colonies need you"
summary stands in for the pile and opens the front page; it closes again once fewer than two remain.

Every device has preferences of its own, kept on the mothership and checked before it sends:

- **Events**: Questions, Pull request opened, Needs rebase, Failed and Needs attention are on by
  default; Provider degraded and the Hourly digest are off until asked for. Devices enrolled before
  preferences existed keep working on these defaults — so Provider degraded and the digest now start
  off for them.
- **Play a sound for questions.** A question is the only push that may sound; everything else
  arrives silent. **Answer buttons on questions** and the **Needs-you count on the app icon** can
  be switched off per device too.
- **Repositories**: empty hears about every colony; entries name an `org` or an `org/repo`, and only
  narrow the events tied to a colony.
- **Quiet hours** hold everything back through the device's own night, with an optional break-through
  for questions. The time zone is stamped when the preferences are saved from that device, and the
  cockpit keeps it fresh while it is open.
- A focused cockpit tab showing a colony holds that device's pushes for the same colony back: the tab
  reports what it is showing every half minute while focused, and a report older than 75 seconds is
  ignored.

Notifications only name the repository and issue number. They never include the issue title, the
question's text, or an error — with one exception: a question the notification itself can answer
carries that question's option labels, so the buttons can name them (below). The question and its
header are never sent.

**Answering from the notification.** When the colony's open question set is exactly one
single-select question with one to three options, its notification shows a button per option, then
**Other…** for a typed reply where the browser delivers one (ChromeOS today), then **Open** while
button slots remain. More options than fit, a multi-select, or several open questions get a single
**Open to answer** button instead. Browsers that show no notification buttons at all — an iPhone or
iPad home-screen app, Firefox, Safari on macOS — fall back to open-on-tap: the tap opens the colony,
where the question card answers as usual. Chrome on Android and desktop show the buttons.

A button answers straight from the service worker with a one-time token the push carried. The token
works for that one question and colony, once, and expires after 24 hours or as soon as the question
is answered or replaced, so a notification is a credential for exactly one answer and nothing else. On success the notification is replaced by a silent
`Answered: <label>` confirmation; on any failure — the question was answered in the cockpit first,
the token expired, no network — the tap opens the colony instead. With several devices subscribed,
the first tap answers and the rest open the cockpit.

Limits: Web Push needs a secure origin (localhost counts) and a browser with push support. On iPhone
and iPad it needs iOS 16.4 or newer, with Colonizer added to the Home Screen; on an iOS device that
isn't installed yet, this pane (and Desktop) shows the Add to Home Screen steps instead. The
"resolved" push shows nothing by design, so it is never sent to Apple endpoints — Safari and iOS
revoke a subscription that receives an invisible push — and there a notification stays until you tap
or clear it, the badge catching up next time the app opens (an installed iOS 16.4+ app can show one).
Chrome and Firefox budget silent pushes, so a resolution that leaves nothing to announce may
occasionally surface their generic "site updated in the background" notice; with two or more
colonies still waiting, the summary notification is visible anyway.

### Desktop

**Settings → Desktop.**

- **Install app** installs the cockpit as an app with its own window and Dock or taskbar icon. The
  button appears when the browser offers installation (Chrome, Edge). In Safari, use File → Add to
  Dock; on an iPhone or iPad the pane shows the Add to Home Screen steps instead. Same cockpit, same
  sign-in. When the mothership ships a new build, a **Colonizer updated** card in the corner offers
  **Reload**; the running build keeps working until you do.
- **Start Colonizer at login** installs a macOS LaunchAgent or a Linux systemd user unit that starts
  the mothership when you log in. Turning it off never stops a running mothership. The same switch
  is `colonizer login-item enable|disable|status`.

### Workspace settings and the agent module

**Settings → Workspaces → a workspace**, or the workspace switcher's "<workspace> settings" row.

Each setting can follow the global default or be overridden for this workspace:

- **Models**: **Agent module** (which agent runs this workspace's colonies), Orchestrator, Subagents
  and Background models. A workspace with no pick of its own uses the global agent module. Claude
  Code is the main one. Codex, OpenCode, Pi, Hermes, Grok Build and ACP modules also exist, but
  several are early: Pi cannot ask you questions, and the Codex and ACP agent binaries are not
  yet staged into the colony image. Each module's description in Settings, Modules says what it
  cannot do. See [runner-authoring.md](runner-authoring.md) for how agent modules work.
- **Colonies**: Stack, parallel colonies, per-repository limit, budget per colony, host disk per
  colony.
- **Memory**: shared memory on or off.
- **Skill sets**: turn individual skill sets on or off for this workspace. Without an override, a
  workspace uses the ones switched on in Settings, Modules. See [skill-packs.md](skill-packs.md).
- **Watchdog**: on or off, minutes before a colony counts as stalled, and nudges before it is
  flagged.

This page is also where you switch a workspace off, or back on.

## Phones and narrow screens

Under 640px wide, the sidebar is replaced by a tab bar at the bottom of the screen with five tabs:
**Nest**, **Inbox**, **Chat**, **Code** and **More**. More opens a sheet with Overview, Launch,
History, Loops, Memory, Host, Secrets and Settings. An open colony counts as Nest. The bar sits
above the phone's home indicator.

⌘K and ⌘B are keyboard shortcuts and do nothing on a phone without a keyboard. Use the Colonize
button on Overview instead. The Colonize pane opens full width over the tab bar.

To reach the cockpit from a phone, turn on [remote access](#remote-access). To be told when a colony
needs you, subscribe the phone to [Web Push](#notifications-and-web-push).

## Keyboard shortcuts

On Linux and Windows, read Ctrl for ⌘.

| Keys | Where | What it does |
| --- | --- | --- |
| ⌘K | Anywhere except the code editor and terminals | Open Colonize |
| ⌘B | Anywhere except text fields | Collapse or expand the sidebar |
| Escape | Colonize | Close, or step back from the confirm step |
| ⌘\ | Chat | Show or hide the conversation list |
| ⌘⇧O | Chat | New conversation |
| ⌘F | Chat, with a conversation open | Search the conversation |
| Escape | Chat | Stop a streaming reply |
| F2 / Delete | Chat conversation list | Rename / delete |
| ⌘⇧F | Code editor | Full screen on or off |
| ↑ ↓ Home End | Workspace switcher | Move between workspaces |
