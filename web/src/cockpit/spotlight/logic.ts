// Spotlight's brain (issue #1218), as pure functions so every rule is tested without a DOM: what a
// query means (go somewhere, do something, ask), the blended and intent-ranked list of results
// (go to / do / issues / ask), the top hit, section cycling, and the recents kept in this browser.
import { searchSettings, type SearchHit } from "../../components/settings/nav";
import type { SectionId } from "../../components/settings/ui";
import type { ChatMeta, Loop, Repo, Session } from "../../types";
import { issuePriority, issueState, type RepoIssue } from "../issuesList";
import type { CockpitView } from "../NavRail";

export type Section = "go" | "do" | "issues" | "ask" | "recent";
export type Intent = "go" | "do" | "ask";

/** What Enter does with a result. */
export type SpotlightAction =
  | { type: "view"; view: CockpitView }
  | { type: "colony"; id: string }
  | { type: "repo"; repo: string }
  | { type: "org"; org: string }
  | { type: "settings"; section: SectionId }
  | { type: "colonize"; repo?: string; search?: string; text?: string }
  /** A write: held for the same approval card a chat's tool call gets. */
  | { type: "propose"; tool: string; args: Record<string, unknown> }
  | { type: "model-switcher" }
  | { type: "chat"; id: string }
  | { type: "new-chat" }
  | { type: "ask"; text: string };

export type IconName = "colony" | "repo" | "org" | "settings" | "view" | "colonize" | "stop" | "resume" | "front" | "model" | "update" | "issue" | "ask" | "chat" | "loop" | "recent";

export interface Result {
  id: string;
  section: Section;
  title: string;
  subtitle?: string;
  icon: IconName;
  /** Words to bold in the title. */
  marks?: string[];
  /** Shown at the right: what kind of thing this is. */
  hint?: string;
  score: number;
  action: SpotlightAction;
  /** A workspace whose avatar leads the row. */
  org?: string;
  tone?: "ok" | "warn" | "err" | "accent";
  /** Whether Enter holds a write for approval, so the row says so. */
  writes?: boolean;
}

// --- What a query means ----------------------------------------------------------------------------

const DO_VERB = /^(colonize|colonise|launch|start|stop|pause|resume|continue|move|front|switch|apply|install|publish|answer|new)\b/i;
const ASK_WORD = /^(why|what|whats|what's|how|when|who|whom|which|where|is|are|was|were|can|could|should|would|does|did|do|will|explain|summari[sz]e|tell|show me|describe|compare|help)\b/i;
const NAV_WORD = /^(go|open|settings?|setting)\b/i;

export function words(query: string): string[] {
  return query.toLowerCase().split(/[\s,]+/).filter(Boolean);
}

/**
 * What the person most likely means. A verb they could act with is a "do"; a question word, a
 * question mark or a longer sentence is an "ask"; anything else is a "go" (find it and open it).
 */
export function classifyIntent(query: string): Intent {
  const q = query.trim();
  if (!q) return "go";
  if (NAV_WORD.test(q)) return "go";
  if (DO_VERB.test(q)) return "do";
  if (q.endsWith("?") || ASK_WORD.test(q)) return "ask";
  return words(q).length >= 5 ? "ask" : "go";
}

// --- Matching --------------------------------------------------------------------------------------

function subsequence(needle: string, text: string): boolean {
  if (needle.length < 3) return false;
  let at = 0;
  for (const ch of text) if (ch === needle[at] && ++at === needle.length) return true;
  return false;
}

/** How well `text` answers every word: 0 for no, up to 100 for a title that starts with the query. */
export function matchScore(text: string, ws: readonly string[], extra = ""): number {
  if (ws.length === 0) return 0;
  const t = text.toLowerCase();
  const x = extra.toLowerCase();
  let total = 0;
  for (const w of ws) {
    let best = 0;
    if (t === w) best = 100;
    else if (t.startsWith(w)) best = 92;
    else if (new RegExp(`(^|[\\s/#._-])${w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`).test(t)) best = 78;
    else if (t.includes(w)) best = 55;
    else if (x.includes(w)) best = 36;
    else if (subsequence(w, t)) best = 18;
    if (best === 0) return 0;
    total += best;
  }
  return total / ws.length;
}

// --- The inputs ------------------------------------------------------------------------------------

export interface Page {
  view: CockpitView;
  label: string;
  words: string;
}

/** The cockpit's own pages, with the words people say for them. */
export const PAGES: readonly Page[] = [
  { view: "home", label: "Nest", words: "home colonies map ants" },
  { view: "overview", label: "Overview", words: "workspaces dashboard everything" },
  { view: "inbox", label: "Inbox", words: "questions decisions needs you attention" },
  { view: "chat", label: "Chat", words: "ask talk conversation assistant" },
  { view: "code", label: "Code", words: "files editor repository browse" },
  { view: "loops", label: "Loops", words: "recurring scheduled cron automation" },
  { view: "history", label: "History", words: "activity log audit past" },
  { view: "memory", label: "Memory", words: "notes remember learned" },
  { view: "host", label: "Host", words: "machine disk cpu storage health" },
  { view: "secrets", label: "Secrets", words: "keys tokens passwords credentials" },
  { view: "launch", label: "Launch form", words: "new colony start manual" },
];

export interface SpotlightData {
  sessions: readonly Session[];
  repos: readonly Repo[];
  /** The workspaces, by org name. */
  orgs: readonly string[];
  issues: readonly RepoIssue[];
  loops: readonly Loop[];
  chats: readonly ChatMeta[];
  /** Settings hits, already searched. */
  settings: readonly { hit: SearchHit; label: string; trail: string }[];
  /** A newer release is out. */
  updateAvailable: boolean;
  /** The org the cockpit is scoped to, or null for every workspace. */
  org: string | null;
  recents: readonly Recent[];
}

const titleOf = (s: Session) => (s.issue_title || s.summary || s.id).trim();
const live = (s: Session) => ["starting", "running", "idle", "waiting_for_answer", "publishing"].includes(s.status);
const short = (repo: string) => repo.split("/")[1] ?? repo;

const STATUS_WORD: Record<string, string> = {
  queued: "queued",
  starting: "starting",
  running: "running",
  waiting_for_answer: "needs you",
  idle: "idle",
  publishing: "publishing",
  pr_opened: "PR open",
  merged: "merged",
  closed: "closed",
  stopped: "stopped",
  failed: "failed",
  no_changes: "no changes",
  parked: "parked",
  blocked: "blocked",
};

function colonySubtitle(s: Session): string {
  return `${s.repo}${s.issue ? `#${s.issue}` : ""} · ${STATUS_WORD[s.status] ?? s.status}`;
}

const tone = (s: Session): Result["tone"] => (s.status === "failed" ? "err" : s.status === "waiting_for_answer" ? "warn" : live(s) ? "accent" : s.status === "merged" || s.status === "pr_opened" ? "ok" : undefined);

// --- Do --------------------------------------------------------------------------------------------

const COLONIZE_ISSUE = /^(?:colonize|colonise|launch|start|do|work(?:\s+on)?)\s+(?:(?<repo>[\w.-]+\/[\w.-]+)\s*)?#?(?<n>\d+)$/i;
const REPO_HASH = /^(?<repo>[\w.-]+\/[\w.-]+)#(?<n>\d+)$/;

/** The repository a bare issue number most likely means: the cockpit's newest repo with that open issue, else the newest repo. */
function reposForNumber(n: number, data: SpotlightData): { repo: string; issue: RepoIssue | null }[] {
  const found = data.issues.filter((i) => i.number === n).map((i) => ({ repo: i.repo, issue: i }));
  if (found.length > 0) return found.slice(0, 3);
  const scope = data.repos.filter((r) => !r.archived && (!data.org || r.full_name.toLowerCase().startsWith(`${data.org.toLowerCase()}/`)));
  const newest = [...scope].sort((a, b) => (b.pushed_at ?? "").localeCompare(a.pushed_at ?? ""))[0];
  return newest ? [{ repo: newest.full_name, issue: null }] : [];
}

function doResults(query: string, ws: string[], data: SpotlightData): Result[] {
  const out: Result[] = [];
  const q = query.trim();
  const add = (r: Omit<Result, "section">) => out.push({ ...r, section: "do" });
  const empty = ws.length === 0;

  // "colonize #212", "colonize acme/web#45": a launch, held for approval.
  const direct = COLONIZE_ISSUE.exec(q);
  const hashed = REPO_HASH.exec(q);
  const target = direct?.groups ? { repo: direct.groups.repo, n: Number(direct.groups.n) } : hashed?.groups ? { repo: hashed.groups.repo, n: Number(hashed.groups.n) } : null;
  if (target) {
    const options = target.repo ? [{ repo: target.repo, issue: data.issues.find((i) => i.repo === target.repo && i.number === target.n) ?? null }] : reposForNumber(target.n, data);
    for (const [i, o] of options.entries())
      add({
        id: `do:colonize:${o.repo}#${target.n}`,
        title: `Colonize ${short(o.repo)} #${target.n}`,
        subtitle: o.issue ? `${o.repo} · ${o.issue.title}` : `${o.repo} · opens a colony on this issue`,
        icon: "colonize",
        score: 120 - i,
        hint: "Colonize",
        writes: true,
        org: o.repo.split("/")[0],
        action: { type: "propose", tool: "launch_colony", args: { repo: o.repo, issue: target.n } },
      });
  }

  // "colonize <something else>": the Colonize pane with that text in the box.
  const free = /^(?:colonize|colonise)\s+(?!#?\d+$)(.+)/i.exec(q);
  if (free)
    add({ id: "do:colonize:text", title: `Colonize: ${free[1]}`, subtitle: "Draft it into issues and send colonies", icon: "colonize", score: 110, hint: "Colonize", action: { type: "colonize", text: free[1] } });

  // Verbs on a colony: stop, resume, front.
  const verb = /^(stop|pause|resume|continue|start|front|move)\b\s*(?:to\s+front)?\s*(.*)$/i.exec(q);
  if (verb) {
    const kind = /^(stop|pause)$/i.test(verb[1]) ? "stop" : /^(resume|continue|start)$/i.test(verb[1]) ? "resume" : "front";
    const rest = words(verb[2].replace(/\b(to\s+)?front\b/i, ""));
    const pool = data.sessions.filter((s) => (kind === "stop" ? live(s) : kind === "resume" ? ["stopped", "parked", "failed"].includes(s.status) : s.status === "queued"));
    const scored = pool
      .map((s) => ({ s, score: rest.length === 0 ? 40 : matchScore(`${titleOf(s)} ${s.repo}`, rest, s.id) }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, 4);
    for (const { s, score } of scored)
      add({
        id: `do:${kind}:${s.id}`,
        title: `${kind === "stop" ? "Stop" : kind === "resume" ? "Resume" : "Move to front:"} ${titleOf(s)}`,
        subtitle: colonySubtitle(s),
        icon: kind === "stop" ? "stop" : kind === "resume" ? "resume" : "front",
        score: 100 + score / 10,
        hint: kind === "stop" ? "Stop" : kind === "resume" ? "Resume" : "Queue",
        writes: true,
        org: s.repo.split("/")[0],
        action: { type: "propose", tool: kind === "stop" ? "stop_colony" : kind === "resume" ? "resume_colony" : "move_to_front", args: { id: s.id } },
      });
  }

  // The standing actions, found by their words.
  const standing: (Omit<Result, "section" | "score"> & { words: string })[] = [
    { id: "do:colonize", title: "Colonize…", subtitle: "Describe new work or pick open issues", icon: "colonize", hint: "Colonize", action: { type: "colonize", repo: undefined }, words: "colonize colonise new issue work task send colonies dispatch" },
    { id: "do:new-chat", title: "New chat", subtitle: "Start a fresh conversation", icon: "chat", hint: "Chat", action: { type: "new-chat" }, words: "new chat conversation ask talk" },
    { id: "do:model", title: "Switch model…", subtitle: "Install-wide or per org", icon: "model", hint: "Models", action: { type: "model-switcher" }, words: "switch model provider glm claude minimax byteplus change" },
  ];
  if (data.updateAvailable)
    standing.push({
      id: "do:update",
      title: "Apply the update",
      subtitle: "Install the newer Colonizer release",
      icon: "update",
      hint: "Update",
      writes: true,
      action: { type: "propose", tool: "apply_update", args: {} },
      words: "update upgrade install release version apply",
    });
  for (const a of standing) {
    const score = empty ? 30 : matchScore(a.title.replace("…", ""), ws, a.words);
    if (score > 0 && !out.some((o) => o.id === a.id)) add({ ...a, score: score * 0.9 });
  }
  return out;
}

// --- Go to -----------------------------------------------------------------------------------------

function goResults(ws: string[], data: SpotlightData): Result[] {
  const out: Result[] = [];
  const add = (r: Omit<Result, "section">) => out.push({ ...r, section: "go" });
  for (const p of PAGES) {
    const score = matchScore(p.label, ws, p.words);
    if (score > 0) add({ id: `go:view:${p.view}`, title: p.label, subtitle: p.words.split(" ").slice(0, 3).join(" · "), icon: "view", hint: "Page", score: score * 0.9, action: { type: "view", view: p.view } });
  }
  for (const s of data.sessions) {
    const score = matchScore(titleOf(s), ws, `${s.repo} ${s.id} ${s.issue ? `#${s.issue}` : ""} ${s.branch}`);
    if (score > 0)
      add({
        id: `go:colony:${s.id}`,
        title: titleOf(s),
        subtitle: colonySubtitle(s),
        icon: "colony",
        hint: "Colony",
        score: score + (live(s) ? 8 : 0) + (data.org && s.repo.toLowerCase().startsWith(`${data.org.toLowerCase()}/`) ? 4 : 0),
        org: s.repo.split("/")[0],
        tone: tone(s),
        action: { type: "colony", id: s.id },
      });
  }
  for (const r of data.repos) {
    if (r.archived) continue;
    const score = matchScore(r.full_name, ws, r.description ?? "");
    if (score > 0) add({ id: `go:repo:${r.full_name}`, title: r.full_name, subtitle: r.description ?? undefined, icon: "repo", hint: "Repository", score: score * 0.95, org: r.full_name.split("/")[0], action: { type: "repo", repo: r.full_name } });
  }
  for (const org of data.orgs) {
    const score = matchScore(org, ws, "workspace organisation organization");
    if (score > 0) add({ id: `go:org:${org}`, title: org, subtitle: "Switch to this workspace", icon: "org", hint: "Workspace", score: score * 0.95, org, action: { type: "org", org } });
  }
  for (const [n, h] of data.settings.entries())
    add({ id: `go:settings:${n}:${h.hit.entry.section}:${h.hit.entry.label}`, title: h.label, subtitle: h.trail, icon: "settings", hint: "Settings", score: Math.min(95, 40 + h.hit.score * 4), action: { type: "settings", section: h.hit.entry.section } });
  for (const l of data.loops) {
    const score = matchScore(l.name, ws, `${l.repo} loop recurring`);
    if (score > 0) add({ id: `go:loop:${l.id}`, title: l.name, subtitle: `${l.repo} · loop`, icon: "loop", hint: "Loop", score: score * 0.9, org: l.repo.split("/")[0], action: { type: "view", view: "loops" } });
  }
  for (const c of data.chats) {
    const score = matchScore(c.title || "New conversation", ws);
    if (score > 0) add({ id: `go:chat:${c.id}`, title: c.title || "New conversation", subtitle: "Conversation", icon: "chat", hint: "Chat", score: score * 0.85, action: { type: "chat", id: c.id } });
  }
  return out;
}

// --- Issues ----------------------------------------------------------------------------------------

function issueResults(ws: string[], data: SpotlightData): Result[] {
  const out: Result[] = [];
  const numeric = ws.length === 1 && /^#?\d+$/.test(ws[0]) ? ws[0].replace("#", "") : null;
  for (const i of data.issues) {
    const hash = String(i.number);
    const score = numeric ? (hash === numeric ? 100 : hash.startsWith(numeric) ? 70 : 0) : matchScore(i.title, ws, `${i.repo} #${hash} ${i.labels.map((l) => l.name).join(" ")}`);
    if (score <= 0) continue;
    const state = issueState(data.sessions, i.repo, i.number);
    const high = issuePriority(i) === "high";
    out.push({
      id: `issue:${i.repo}#${i.number}`,
      section: "issues",
      title: i.title,
      subtitle: `${i.repo}#${i.number}${state.status === "new" ? "" : ` · ${state.status === "pr_open" ? "PR open" : state.status === "blocked" ? "needs you" : state.status}`}`,
      icon: "issue",
      hint: high ? "High priority" : "Issue",
      score: score - (state.status === "new" ? 0 : 6) + (high ? 3 : 0),
      org: i.repo.split("/")[0],
      tone: state.status === "blocked" ? "warn" : undefined,
      action: { type: "colonize", repo: i.repo, search: `#${i.number}` },
    });
  }
  return out;
}

// --- The list --------------------------------------------------------------------------------------

export const SECTION_TITLE: Record<Section, string> = { go: "Go to", do: "Do", issues: "Issues", ask: "Ask", recent: "Recent" };

/** How many rows a section may show. */
const LIMIT: Record<Section, number> = { go: 6, do: 5, issues: 4, ask: 1, recent: 6 };

export interface Listing {
  results: Result[];
  intent: Intent;
  /** Where Enter lands first: by intent. */
  top: number;
}

/** The ask row: always last. */
export function askRow(query: string): Result {
  return { id: "ask", section: "ask", title: `Ask Colonizer: ${query.trim()}`, icon: "ask", hint: "Ask", score: 0, action: { type: "ask", text: query.trim() } };
}

/** Everything the box shows for a query, in section order, with the row Enter should pick. */
export function buildListing(query: string, data: SpotlightData): Listing {
  const q = query.trim();
  if (!q) {
    const recents: Result[] = data.recents.slice(0, LIMIT.recent).map((r, i) => ({
      id: `recent:${r.id}`,
      section: "recent",
      title: r.title,
      subtitle: r.subtitle,
      icon: r.icon,
      hint: r.hint,
      score: 100 - i,
      org: r.org,
      action: r.action,
    }));
    const suggestions = doResults("", [], data).slice(0, 3).map((r) => ({ ...r, section: "do" as const }));
    const results = [...recents, ...suggestions];
    return { results, intent: "go", top: 0 };
  }
  const ws = words(q);
  const intent = classifyIntent(q);
  const by = (list: Result[], section: Section) => list.sort((a, b) => b.score - a.score || a.title.localeCompare(b.title)).slice(0, LIMIT[section]);
  const go = by(goResults(ws, data), "go");
  const doing = by(doResults(q, ws, data), "do");
  const issues = by(issueResults(ws, data), "issues");
  // A bare number is an issue lookup: the issues lead.
  // The section the person most likely wants leads; the ask row is always last.
  const results = intent === "do" ? [...doing, ...issues, ...go, askRow(q)] : [...go, ...doing, ...issues, askRow(q)];
  const first = (section: Section) => results.findIndex((r) => r.section === section);
  let top: number;
  if (intent === "ask") top = results.length - 1;
  else if (intent === "do") top = first("do") >= 0 ? first("do") : Math.max(0, first("go"));
  else top = first("go") >= 0 ? first("go") : first("issues") >= 0 ? first("issues") : first("do") >= 0 ? first("do") : results.length - 1;
  // "colonize #212" lands on its launch, and a bare "#212" on the issue.
  return { results, intent, top: Math.max(0, top) };
}

/** Tab: the first row of the next (or, with shift, the previous) section. */
export function nextSection(results: readonly Result[], at: number, back = false): number {
  const sections = [...new Set(results.map((r) => r.section))];
  if (sections.length === 0) return 0;
  const here = sections.indexOf(results[at]?.section ?? sections[0]);
  const next = sections[(here + (back ? -1 : 1) + sections.length) % sections.length];
  return Math.max(0, results.findIndex((r) => r.section === next));
}

/** Arrow keys wrap. */
export function moveSelection(at: number, count: number, delta: number): number {
  return count === 0 ? 0 : (at + delta + count) % count;
}

// --- Recents ---------------------------------------------------------------------------------------

export interface Recent {
  id: string;
  title: string;
  subtitle?: string;
  icon: IconName;
  hint?: string;
  org?: string;
  action: SpotlightAction;
  ts: number;
}

export const RECENTS_KEY = "colonizer.spotlight.recents";
const MAX_RECENTS = 12;

export function loadRecents(raw: string | null): Recent[] {
  try {
    const list: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(list) ? list.filter((r): r is Recent => typeof r?.id === "string" && typeof r?.title === "string" && typeof r?.action?.type === "string") : [];
  } catch {
    return [];
  }
}

/** A result remembered: newest first, one per id, the ask row as the question itself. */
export function remember(recents: readonly Recent[], r: Result, now: number): Recent[] {
  const id = r.action.type === "ask" ? `ask:${r.action.text.toLowerCase()}` : r.id;
  const entry: Recent = {
    id,
    title: r.action.type === "ask" ? r.action.text : r.title,
    subtitle: r.action.type === "ask" ? "Asked" : r.subtitle,
    icon: r.action.type === "ask" ? "recent" : r.icon,
    hint: r.action.type === "ask" ? "Ask" : r.hint,
    org: r.org,
    action: r.action,
    ts: now,
  };
  return [entry, ...recents.filter((x) => x.id !== id)].slice(0, MAX_RECENTS);
}

/** The most recent question, which Up recalls into an empty box; `back` steps further into the past. */
export function recallAsk(recents: readonly Recent[], back = 0): string | null {
  const asks = recents.filter((r) => r.action.type === "ask");
  const hit = asks[Math.min(back, asks.length - 1)];
  return hit && hit.action.type === "ask" ? hit.action.text : null;
}

/** Settings pages and fields for a query, in the shape the listing wants. */
export function settingsHits(index: Parameters<typeof searchSettings>[0], crumbsOf: Parameters<typeof searchSettings>[1], query: string): SpotlightData["settings"] {
  return searchSettings(index, crumbsOf, query.replace(/^\s*(settings?|go to|open)\s+/i, ""), 5).map((hit) => {
    const crumbs = crumbsOf(hit.entry);
    return { hit, label: hit.entry.label, trail: crumbs.slice(0, -1).join(" › ") || hit.entry.help };
  });
}
