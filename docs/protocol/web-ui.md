# Web UI

Part of the [Colonizer protocol](../protocol.md).

## 5. Web UI contract

- Stack: Vite + React + TypeScript + Tailwind v4 + assistant-ui (`useExternalStoreRuntime`) + xterm.js.
  Built to `web/dist`; dev server proxies `/api` (incl. WebSockets) to `http://127.0.0.1:7878`.
- Layout: the cockpit (`web/src/cockpit/Cockpit.tsx`) is one page with a navigation rail (a tab bar
  on a phone) and these views: overview, the nest, launch, a colony view (status, branch, cost, host
  disk, the actions Create PR, Stop, Resume, Clean up; chat beside a terminal), Code, Chat, Loops,
  Secrets, Host, Inbox (behind the bell) and History, plus memory and settings. The Settings dialog
  has Setup, Connections, Model providers, Runtime, Live map, Remote access, Add your phone, API
  tokens, Fleet, Updates, Usage data, Notifications and Desktop, the module settings, and one entry
  per org workspace.
- Events → assistant-ui messages: `user_message` → user message; `assistant_text(_delta)`, `thinking`,
  `tool_call` + `tool_result` → parts of the current assistant message; `question` → a tool-call part
  with `toolName: "ask_user"` rendered by a registered tool UI.
- Consecutive assistant messages group into one bubble, and a change of `agent` breaks the group, so a
  subagent's turn is never folded into the orchestrator's. A subagent's bubble is shown as its own
  speaker: ant avatar, the subagent's name, indented under the `Task` call that started it.
- Choice card (`ask_user`): one section per question with its header chip; options as large selectable
  cards (radio, or checkboxes when `multi_select`) showing label + description; `preview` rendered as
  monospace text (markdown) or a sandboxed `iframe srcdoc` (HTML); an always-present "Other…" option
  with a text field; a single Submit button that sends `answer`. Once `question_answered` arrives the
  card collapses to a summary of the chosen answers.
- The composer sends `user_message`; a Stop button sends `interrupt` while the agent is working.
- Offline: with a colony's events socket down, an answer or a `user_message` the composer cannot
  send is queued in the service worker (`web/public/sw-outbox.js`, IndexedDB) and sent one at a
  time, in order, through the two HTTP twins above — Background Sync (tag `colonizer-outbox`) where
  the browser offers it, otherwise on the next open, `online` or return to the tab. A queued answer
  carries `questions`, the question content the operator read: the answer route refuses it (**409**)
  unless the open question still has that id and that content, and the first delivery closes the
  question, so a replay is delivered at most once and never to a changed question. A queued message
  carries its item id as the dedupe `id`. A final refusal — a 4xx other than an auth 401/403 — drops
  the item with a note; a network error, a 5xx or an auth 401/403 keeps it; an item queued over a day
  ago is dropped unsent.
- Light and dark themes via `prefers-color-scheme`; usable at 400 px width.

---

## 6.4 UI additions

- Sidebar org switcher (All orgs, then each org) filtering repositories and colonies; org chip on
  colonies; per-org settings dialog with an "inherit" state for every field.
- Settings → Model providers: add from the preset catalogue (DeepSeek, OpenAI, Z.ai, Alibaba, Local) or custom, base URL, auth,
  key (write-only), model list. Agent module model fields get suggestions from `GET /api/models`.
- Memory view with pending proposals (approve, edit then approve, reject), notes per scope (global, org,
  repo), and a pending count badge in the sidebar.
- Watchdog: amber attention badge on colonies, the reason in the colony header, and `watchdog-` messages
  rendered as notices.
