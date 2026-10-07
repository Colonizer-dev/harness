// Spotlight (issue #1218): one bar, from anywhere, that understands what you mean. ⌘K or `/`, the
// pill in the top bar, or the phone's floating button (or a pull down) open the same frosted panel
// over the current page. As you type, one ranked list blends where you can go, what you can do,
// the open issues, and an "Ask Colonizer" row that streams its answer inline and expands into a
// full thread on the Chat page. Anything it does that changes something goes through the same
// approval card the chat's tools use (#1217), so nothing runs without a click.
import { createContext, useCallback, useContext, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type ReactElement, type ReactNode } from "react";
import { createPortal } from "react-dom";

import { errorMessage, useApi, useToast } from "../../context";
import type { SectionId } from "../../components/settings/ui";
import { IconSearch } from "../../components/icons";
import { Spinner, cx, store, stored } from "../../components/ui";
import type { ChatApproval, ChatToolNote, Repo, Session } from "../../types";
import { ChatMarkdown } from "../chat/ChatMarkdown";
import { ApprovalCard, ToolCalls, useApprovals } from "../chat/ToolCalls";
import { AntGlyph } from "../chat/PersonaAnt";
import { useColonize } from "../Colonize";
import { openModelSwitcher } from "../ModelSwitcher";
import type { CockpitView } from "../NavRail";
import {
  RECENTS_KEY,
  SECTION_TITLE,
  buildListing,
  loadRecents,
  moveSelection,
  nextSection,
  recallAsk,
  remember,
  settingsHits,
  words,
  type Recent,
  type Result,
  type Section,
  type SpotlightAction,
  type SpotlightData,
} from "./logic";
import { Key, MOD, ResultRow, SpotlightFab, SpotlightPill } from "./Parts";
import { useSpotlightData } from "./useSpotlightData";
import "./spotlight.css";

/** What Spotlight needs from the cockpit around it: the things to search, and how to go to each. */
export interface SpotlightHost {
  sessions: readonly Session[];
  repos: readonly Repo[];
  /** The workspaces, by org name. */
  orgs: readonly string[];
  /** The workspace the cockpit is scoped to. */
  org: string | null;
  view: CockpitView;
  /** The colony on screen, which an ask is about. */
  colony: Session | null;
  updateAvailable: boolean;
  onNavigate: (view: CockpitView) => void;
  onOpenColony: (id: string) => void;
  onOpenSettings: (section: SectionId) => void;
  onSelectOrg: (org: string) => void;
  onOpenRepo: (repo: string) => void;
  /** Opens a conversation on the Chat page, or a fresh one with `null`. */
  onOpenChat: (id: string | null) => void;
}

interface Handle {
  open: (query?: string) => void;
  isOpen: boolean;
}

const SpotlightContext = createContext<Handle | null>(null);

/** Opens Spotlight from anywhere under the provider; null outside one. */
export function useSpotlight(): Handle | null {
  return useContext(SpotlightContext);
}

/** The top bar's pill, wired to the surrounding provider; nothing outside one. */
export function SpotlightSearch(): ReactElement | null {
  const spotlight = useSpotlight();
  return spotlight ? <SpotlightPill onOpen={() => spotlight.open()} /> : null;
}

/** Whether a key press is meant for Spotlight and not for a field, the editor or a terminal. */
export interface KeyLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  defaultPrevented: boolean;
  target: EventTarget | null;
}

export function isSpotlightKey(event: KeyLike, slashOwned = false): boolean {
  if (event.defaultPrevented || event.altKey) return false;
  const target = event.target as (Partial<HTMLElement> & { closest?: (selector: string) => unknown }) | null;
  if (event.key.toLowerCase() === "k" && (event.metaKey || event.ctrlKey) && !event.shiftKey) return !target?.closest?.(".monaco-editor, .xterm");
  if (event.key === "/" && !event.metaKey && !event.ctrlKey && !slashOwned) {
    if (target?.isContentEditable) return false;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(target?.tagName ?? "")) return false;
    return !target?.closest?.(".monaco-editor, .xterm, dialog[open], [contenteditable='true']");
  }
  return false;
}

/** How far down a drag from the top of the page opens Spotlight, in px. */
export const PULL_DISTANCE = 84;

/** Whether the touch began somewhere that cannot scroll up further, so a pull down is the page's own. */
function atTop(el: Element | null): boolean {
  for (let node: Element | null = el; node && node !== document.documentElement; node = node.parentElement) {
    if (node.scrollTop > 0) return false;
    if (node.matches?.("textarea, input, .monaco-editor, .xterm, dialog, [role='dialog']")) return false;
  }
  return true;
}

export function SpotlightProvider({ host, children }: { host: SpotlightHost; children: ReactNode }): ReactElement {
  const [isOpen, setOpen] = useState(false);
  const [seed, setSeed] = useState("");
  const opener = useRef<Element | null>(null);
  const hostRef = useRef(host);
  hostRef.current = host;

  const open = useCallback((query = "") => {
    opener.current = typeof document === "undefined" ? null : document.activeElement;
    setSeed(query);
    setOpen(true);
  }, []);
  const close = useCallback(() => {
    setOpen(false);
    const el = opener.current;
    if (el instanceof HTMLElement) setTimeout(() => el.focus?.(), 0);
  }, []);

  // ⌘K / Ctrl-K, and `/` anywhere that is not a text field. Settings keeps its own `/`.
  useEffect(() => {
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (!isSpotlightKey(event, hostRef.current.view === "settings")) return;
      event.preventDefault();
      setOpen((now) => {
        if (now) return false;
        opener.current = document.activeElement;
        setSeed("");
        return true;
      });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // A pull down from the top of the page, on a phone.
  useEffect(() => {
    let start: { y: number; x: number; el: Element | null } | null = null;
    const down = (e: TouchEvent) => {
      const t = e.touches[0];
      start = e.touches.length === 1 && t.clientY < 140 ? { y: t.clientY, x: t.clientX, el: e.target as Element | null } : null;
    };
    const move = (e: TouchEvent) => {
      if (!start) return;
      const t = e.touches[0];
      const dy = t.clientY - start.y;
      if (Math.abs(t.clientX - start.x) > dy * 0.6) {
        start = null;
        return;
      }
      if (dy >= PULL_DISTANCE && atTop(start.el)) {
        start = null;
        open();
      }
    };
    const end = () => {
      start = null;
    };
    window.addEventListener("touchstart", down, { passive: true });
    window.addEventListener("touchmove", move, { passive: true });
    window.addEventListener("touchend", end, { passive: true });
    return () => {
      window.removeEventListener("touchstart", down);
      window.removeEventListener("touchmove", move);
      window.removeEventListener("touchend", end);
    };
  }, [open]);

  const handle = useMemo<Handle>(() => ({ open, isOpen }), [open, isOpen]);
  return (
    <SpotlightContext.Provider value={handle}>
      {children}
      {!isOpen && <SpotlightFab onOpen={() => open()} />}
      {isOpen && <SpotlightPanel host={host} seed={seed} onClose={close} />}
    </SpotlightContext.Provider>
  );
}

type Mode = { kind: "search" } | { kind: "answer"; question: string; n: number } | { kind: "approval"; approval: ChatApproval };

/** The panel itself. Exported for the tests, which render it with a host and an api. */
export function SpotlightPanel({ host, seed = "", onClose }: { host: SpotlightHost; seed?: string; onClose: () => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const colonize = useColonize();
  const listId = useId();
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState(seed);
  const [mode, setMode] = useState<Mode>({ kind: "search" });
  const [chat, setChat] = useState<string | null>(null);
  const [recents, setRecents] = useState<Recent[]>(() => loadRecents(stored(RECENTS_KEY)));
  const [recall, setRecall] = useState(0);
  const [pickedId, setPickedId] = useState<string | null>(null);
  const loaded = useSpotlightData(true, host.repos, host.org, host.orgs);

  const data = useMemo<SpotlightData>(
    () => ({
      sessions: host.sessions,
      repos: host.repos,
      orgs: host.orgs,
      issues: loaded.issues,
      loops: loaded.loops,
      chats: loaded.chats,
      settings: query.trim() ? settingsHits(loaded.index, loaded.crumbsOf, query) : [],
      updateAvailable: host.updateAvailable,
      org: host.org,
      recents,
    }),
    [host.sessions, host.repos, host.orgs, host.updateAvailable, host.org, loaded.issues, loaded.loops, loaded.chats, loaded.index, loaded.crumbsOf, query, recents],
  );
  const listing = useMemo(() => buildListing(query, data), [query, data]);
  const found = pickedId === null ? -1 : listing.results.findIndex((r) => r.id === pickedId);
  const at = found >= 0 ? found : listing.top;
  const selected: Result | undefined = listing.results[at];
  const ws = useMemo(() => words(query), [query]);

  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);
  useEffect(() => setPickedId(null), [query]);
  // Escape from anywhere in the panel, including a focused approval button.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      if (mode.kind === "search") onClose();
      else {
        setMode({ kind: "search" });
        input.current?.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [mode.kind, onClose]);

  const done = (result: Result) => {
    const next = remember(recents, result, Date.now());
    setRecents(next);
    store(RECENTS_KEY, JSON.stringify(next));
  };

  const go = (action: SpotlightAction, result: Result) => {
    switch (action.type) {
      case "ask":
        done(result);
        setMode({ kind: "answer", question: action.text, n: Date.now() });
        return;
      case "propose":
        done(result);
        void api.proposeApproval(action.tool, action.args).then(
          (approval) => setMode({ kind: "approval", approval }),
          (e) => toast(errorMessage(e), "error"),
        );
        return;
      case "view":
        host.onNavigate(action.view);
        break;
      case "colony":
        host.onOpenColony(action.id);
        break;
      case "repo":
        host.onOpenRepo(action.repo);
        break;
      case "org":
        host.onSelectOrg(action.org);
        break;
      case "settings":
        host.onOpenSettings(action.section);
        break;
      case "colonize":
        colonize?.open({ repo: action.repo, search: action.search, text: action.text });
        break;
      case "model-switcher":
        openModelSwitcher();
        break;
      case "chat":
        host.onOpenChat(action.id);
        break;
      case "new-chat":
        host.onOpenChat(null);
        break;
    }
    done(result);
    onClose();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    const count = listing.results.length;
    if (e.nativeEvent.isComposing) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (mode.kind !== "search") return setMode({ kind: "search" });
      if (e.key === "ArrowUp" && query === "" && recall >= 0) {
        // Up in an empty box recalls the last question, and keeps stepping back through them.
        const text = recallAsk(recents, recall);
        if (text) {
          setQuery(text);
          setRecall((r) => r + 1);
          return;
        }
      }
      setPickedId(listing.results[moveSelection(at, count, e.key === "ArrowDown" ? 1 : -1)]?.id ?? null);
    } else if (e.key === "Tab" && mode.kind === "search" && count > 0) {
      e.preventDefault();
      setPickedId(listing.results[nextSection(listing.results, at, e.shiftKey)]?.id ?? null);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (mode.kind === "answer" && (e.metaKey || e.ctrlKey) && chat) {
        host.onOpenChat(chat);
        onClose();
      } else if (mode.kind === "search" || query.trim() !== (mode.kind === "answer" ? mode.question : "")) {
        if (mode.kind !== "search") {
          const q = query.trim();
          if (q) go({ type: "ask", text: q }, { id: "ask", section: "ask", title: q, icon: "ask", score: 0, action: { type: "ask", text: q } });
        } else if (selected) go(selected.action, selected);
      }
    } else if (e.key === "Backspace" && query === "" && mode.kind !== "search") {
      setMode({ kind: "search" });
    }
  };

  const sections: { section: Section; rows: { result: Result; index: number }[] }[] = [];
  listing.results.forEach((result, index) => {
    const last = sections.at(-1);
    if (last && last.section === result.section) last.rows.push({ result, index });
    else sections.push({ section: result.section, rows: [{ result, index }] });
  });

  const showList = mode.kind === "search";
  const scope = host.colony ? (host.colony.issue_title || host.colony.summary || host.colony.id) : (host.org ?? "All workspaces");
  const body = (
    <div className="fixed inset-0 z-[70]" data-testid="spotlight">
      <div aria-hidden="true" className="spot-backdrop absolute inset-0" onClick={onClose} />
      <div className="pointer-events-none absolute inset-x-0 top-0 flex justify-center px-2 pt-[max(0.5rem,env(safe-area-inset-top))] sm:px-4 sm:pt-[11vh]">
        <div
          role="dialog"
          aria-modal="true"
          aria-label="Ask or search"
          className="spot-panel pointer-events-auto flex max-h-[calc(100dvh-1rem)] w-full max-w-[680px] flex-col overflow-hidden rounded-[22px] text-text sm:max-h-[78vh]"
        >
          <div className="flex shrink-0 items-center gap-3 px-4 py-3.5 sm:px-5 sm:py-4">
            <span aria-hidden="true" className="grid size-6 shrink-0 place-items-center text-muted">
              {mode.kind === "answer" ? <AntGlyph size={26} /> : <IconSearch size={20} />}
            </span>
            <input
              ref={input}
              role="combobox"
              aria-expanded={showList}
              aria-controls={listId}
              aria-activedescendant={showList && selected ? `${listId}-${at}` : undefined}
              aria-autocomplete="list"
              aria-label="Ask or search"
              autoComplete="off"
              autoCorrect="off"
              spellCheck={false}
              enterKeyHint="go"
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setRecall(0);
                if (mode.kind !== "search" && e.target.value.trim() === "") setMode({ kind: "search" });
              }}
              onKeyDown={onKeyDown}
              placeholder={mode.kind === "answer" ? "Ask a follow-up…" : "Ask or search…"}
              className="spot-input bare-field min-w-0 flex-1 border-0 bg-transparent p-0 text-text outline-none focus-visible:outline-none"
            />
            {loaded.loadingIssues && <Spinner className="size-3.5 shrink-0 text-faint" />}
            <span title="Where this applies" className="hidden max-w-[40%] shrink-0 truncate rounded-full bg-panel-3/80 px-2.5 py-1 text-small text-muted sm:inline">
              {scope}
            </span>
            <button type="button" onClick={onClose} aria-label="close" className="shrink-0 cursor-pointer border-0 bg-transparent p-0">
              <Key>esc</Key>
            </button>
          </div>

          <div className="spot-collapse min-h-0" data-open={showList ? "true" : "false"}>
            <div className="min-h-0">
              {showList && (
                <div id={listId} role="listbox" aria-label="results" className="scroll-thin max-h-[min(54vh,470px)] overflow-y-auto border-t border-border px-2 pb-2 pt-1 max-sm:max-h-[calc(100dvh-10.5rem)]">
                  {sections.map(({ section, rows }) => (
                    <div key={section} role="group" aria-label={SECTION_TITLE[section]} className="spot-section">
                      <div className="px-2.5 pb-1 pt-2.5 text-meta-lg font-semibold uppercase tracking-[0.07em] text-faint">{SECTION_TITLE[section]}</div>
                      {rows.map(({ result, index }) => (
                        <ResultRow
                          key={result.id}
                          id={`${listId}-${index}`}
                          result={result}
                          words={ws}
                          selected={index === at}
                          top={index === listing.top && query.trim() !== "" && result.section !== "ask"}
                          onHover={() => index !== at && setPickedId(result.id)}
                          onPick={() => go(result.action, result)}
                        />
                      ))}
                    </div>
                  ))}
                  {query.trim() !== "" && loaded.loadingIssues && !sections.some((s) => s.section === "issues") && <p className="m-0 px-3 py-2 text-small text-faint">Looking through open issues…</p>}
                </div>
              )}
            </div>
          </div>

          {mode.kind === "answer" && <AnswerPane host={host} question={mode.question} n={mode.n} chat={chat} onChat={setChat} onExpand={(id) => (host.onOpenChat(id), onClose())} />}
          {mode.kind === "approval" && <ApprovalPane approval={mode.approval} onClose={onClose} />}

          <footer className="flex shrink-0 items-center gap-4 border-t border-border bg-panel-2/40 px-4 py-2 text-small text-faint max-sm:hidden">
            {mode.kind === "search" ? (
              <>
                <span className="inline-flex items-center gap-1.5">
                  <Key>↑</Key>
                  <Key>↓</Key> select
                </span>
                <span className="inline-flex items-center gap-1.5">
                  <Key>↵</Key> {selected?.section === "ask" ? "ask" : selected?.writes ? "review" : "open"}
                </span>
                <span className="inline-flex items-center gap-1.5">
                  <Key>⇥</Key> next section
                </span>
              </>
            ) : mode.kind === "answer" ? (
              <>
                <span className="inline-flex items-center gap-1.5">
                  <Key>{MOD}</Key>
                  <Key>↵</Key> expand to chat
                </span>
                <span className="inline-flex items-center gap-1.5">
                  <Key>esc</Key> back
                </span>
              </>
            ) : (
              <span className="inline-flex items-center gap-1.5">
                <Key>esc</Key> back
              </span>
            )}
            <span className="ml-auto">Colonizer</span>
          </footer>
        </div>
      </div>
    </div>
  );
  return typeof document === "undefined" ? body : createPortal(body, document.body);
}

/** A held write that Spotlight itself proposed, with the same card a chat's tool call gets. */
function ApprovalPane({ approval, onClose }: { approval: ChatApproval; onClose: () => void }): ReactElement {
  const approvals = useApprovals(null);
  const { add } = approvals;
  useEffect(() => add(approval), [add, approval]);
  const shown = approvals.byId[approval.id] ?? approval;
  return (
    <div className="scroll-thin max-h-[min(60vh,520px)] overflow-y-auto border-t border-border p-3.5 sm:p-4">
      <ApprovalCard approval={shown} busy={approvals.busy.has(approval.id)} onDecide={(body) => void approvals.decide(approval.id, body)} />
      {(shown.status === "approved" || shown.status === "rejected" || shown.status === "failed") && (
        <div className="mt-3 flex justify-end">
          <button type="button" onClick={onClose} className="h-8 cursor-pointer rounded-lg border border-border bg-transparent px-3 text-small-lg text-text hover:bg-panel-2">
            Done
          </button>
        </div>
      )}
    </div>
  );
}

/** The answer to a question, streaming in under the box. */
function AnswerPane({
  host,
  question,
  n,
  chat,
  onChat,
  onExpand,
}: {
  host: SpotlightHost;
  question: string;
  n: number;
  chat: string | null;
  onChat: (id: string) => void;
  onExpand: (id: string) => void;
}): ReactElement {
  const api = useApi();
  const [text, setText] = useState("");
  const [tools, setTools] = useState<ChatToolNote[]>([]);
  const [phase, setPhase] = useState<"thinking" | "streaming" | "done" | "error">("thinking");
  const [error, setError] = useState<string | null>(null);
  const approvals = useApprovals(chat);
  const { add: addApproval } = approvals;
  const abort = useRef<AbortController | null>(null);
  const chatRef = useRef(chat);
  chatRef.current = chat;
  const colonyId = host.colony?.id;
  const org = host.org;

  useEffect(() => {
    const controller = new AbortController();
    abort.current = controller;
    setText("");
    setTools([]);
    setError(null);
    setPhase("thinking");
    // Started a tick late, so a development double-mount cancels the first run before it sends.
    const timer = setTimeout(() => void (async () => {
      try {
        let id = chatRef.current;
        if (!id) {
          const models = await api.chatModels();
          if (!models.default) throw new Error("No model is reachable yet. Add a provider under Settings → Model providers.");
          const meta = await api.createChat({ model: models.default, workspace: org ?? undefined });
          id = meta.id;
          onChat(id);
        }
        await api.sendChat(
          id,
          { content: question, attachments: colonyId ? [{ kind: "colony", id: colonyId }] : [] },
          (event) => {
            if (event.type === "delta") {
              setPhase("streaming");
              setText((t) => t + event.text);
            } else if (event.type === "tool") {
              if (event.approval) addApproval(event.approval);
              setTools((list) => [...list, event.note]);
            } else if (event.type === "done") {
              setPhase("done");
            } else if (event.type === "error") {
              setError(event.message);
              setPhase("error");
            }
          },
          controller.signal,
        );
        if (!controller.signal.aborted) setPhase((p) => (p === "error" ? p : "done"));
      } catch (e) {
        if (!controller.signal.aborted) {
          setError(errorMessage(e));
          setPhase("error");
        }
      }
    })(), 0);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
    // A new question (a fresh `n`) starts a new answer in the same conversation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [n]);

  const busy = phase === "thinking" || phase === "streaming";
  return (
    <div className="spot-answer scroll-thin max-h-[min(58vh,540px)] overflow-y-auto border-t border-border px-4 pb-3 pt-3 sm:px-5" aria-live="polite" aria-busy={busy}>
      <p className="m-0 mb-2 truncate text-small text-faint">Asked: {question}</p>
      {text ? (
        <ChatMarkdown text={text} live={busy} />
      ) : phase === "thinking" && tools.length === 0 ? (
        <span className="chat-dots inline-flex gap-1 py-2" aria-label="thinking">
          <i />
          <i />
          <i />
        </span>
      ) : null}
      {tools.length > 0 && <ToolCalls notes={tools} approvals={approvals} />}
      {error && (
        <p role="alert" className="m-0 mt-2 rounded-lg border border-err/30 bg-err/5 px-2.5 py-1.5 text-small-lg text-err">
          {error}
        </p>
      )}
      <div className="mt-3 flex items-center gap-2">
        {busy ? (
          <button type="button" onClick={() => abort.current?.abort()} className="h-8 cursor-pointer rounded-lg border border-border bg-transparent px-3 text-small-lg text-muted hover:text-text">
            Stop
          </button>
        ) : null}
        {chat && (phase === "done" || phase === "error") && (
          <button
            type="button"
            onClick={() => onExpand(chat)}
            className={cx("inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-panel-2/70 px-3 text-small-lg text-text transition-colors hover:bg-panel-3")}
          >
            Expand to chat
            <span className="flex gap-0.5">
              <Key>{MOD}</Key>
              <Key>↵</Key>
            </span>
          </button>
        )}
      </div>
    </div>
  );
}
