// The Ask panel (issue #1228): chat from anywhere. A round button at the bottom right of every view
// but Chat, or ⌘J / Ctrl-J, opens a compact Spotlight panel docked above it: the conversation, an
// input, the same approval cards a chat's tools raise, and "Open full chat". The page you are on
// rides along as a chip (the colony you are looking at, else the workspace), and the last
// conversation is kept while the panel is closed, so reopening picks up where it left off.
//
// The conversation lives in the provider, not the panel, so an answer keeps streaming while the panel
// is closed. It uses the chat API the Ask row in Spotlight uses (createChat, then sendChat).
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode, type Ref, type RefObject } from "react";

import { IconChat } from "../../components/icons";
import { errorMessage, useApi } from "../../context";
import { cx } from "../../components/ui";
import type { ChatToolNote, Session } from "../../types";
import { ChatMarkdown } from "../chat/ChatMarkdown";
import { AntGlyph } from "../chat/PersonaAnt";
import { ToolCalls, useApprovals } from "../chat/ToolCalls";
import { PanelFooter, PanelFrame, PanelInput, PanelList, usePanelNav, type PanelRow, type PanelSection } from "./Panel";
import { MOD, Tile } from "./Parts";
import type { SpotlightHost } from "./Spotlight";

/** One question and what came back. */
export interface AskTurn {
  id: number;
  question: string;
  /** The colony it was about, when one was attached. */
  colony: string | null;
  text: string;
  tools: ChatToolNote[];
  phase: "thinking" | "streaming" | "done" | "error";
  error: string | null;
}

/** Whether a key press is ⌘J / Ctrl-J: not in the code editor or a terminal, which own it. */
export function isAskKey(event: { key: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey: boolean; defaultPrevented: boolean; target: EventTarget | null }): boolean {
  if (event.defaultPrevented || event.altKey || event.shiftKey || event.key.toLowerCase() !== "j" || !(event.metaKey || event.ctrlKey)) return false;
  const target = event.target as { closest?: (selector: string) => unknown } | null;
  return !target?.closest?.(".monaco-editor, .xterm");
}

/** Whether the floating button shows on this view: everywhere but the Chat page, which is the same thing in full. */
export function askButtonShown(view: string): boolean {
  return view !== "chat";
}

/** What the panel attaches: the colony asked about, else the colony on screen, else nothing (the workspace goes along as the chat's scope). */
export function askColony(override: Session | null | undefined, onScreen: Session | null): Session | null {
  return override === undefined ? onScreen : override;
}

/** The label of the context chip. */
export function askContextLabel(colony: Session | null, org: string | null): string {
  if (colony) return `${colony.repo}${colony.issue != null ? `#${colony.issue}` : ""}`;
  return org ?? "All workspaces";
}

interface AskHandle {
  /** Opens the panel; with a colony, asking about that one. */
  open: (options?: { colony?: Session }) => void;
  close: () => void;
  toggle: () => void;
  isOpen: boolean;
}

const AskContext = createContext<AskHandle | null>(null);

/** The Ask panel's handle, or null outside an AskProvider. */
export function useAsk(): AskHandle | null {
  return useContext(AskContext);
}

const SUGGESTIONS = [
  { id: "needs", text: "What needs me right now?", hint: "Questions and approvals waiting on you" },
  { id: "colony", text: "Summarize this colony", hint: "Where it is and what it needs", colonyOnly: true },
  { id: "stuck", text: "Which colonies look stuck?", hint: "Quiet for a while, or failing" },
  { id: "spend", text: "How much have we spent today?", hint: "Across the workspace" },
];

export function AskProvider({ host, children }: { host: SpotlightHost; children: ReactNode }): ReactElement {
  const api = useApi();
  const [isOpen, setOpen] = useState(false);
  // undefined follows the page; a colony was asked about; null means the person took the chip off.
  const [override, setOverride] = useState<Session | null | undefined>(undefined);
  const [turns, setTurns] = useState<AskTurn[]>([]);
  const [chat, setChat] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const chatRef = useRef<string | null>(null);
  chatRef.current = chat;
  const hostRef = useRef(host);
  hostRef.current = host;
  const abort = useRef<AbortController | null>(null);
  const approvals = useApprovals(chat);
  const { add: addApproval } = approvals;
  const next = useRef(1);

  const open = useCallback((options?: { colony?: Session }) => {
    if (options?.colony) setOverride(options.colony);
    setOpen(true);
  }, []);
  const close = useCallback(() => setOpen(false), []);
  const toggle = useCallback(() => setOpen((o) => !o), []);

  // ⌘J / Ctrl-J, from anywhere but the editor and a terminal.
  useEffect(() => {
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (!isAskKey(event) || !askButtonShown(hostRef.current.view)) return;
      event.preventDefault();
      setOpen((o) => !o);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // On the Chat page the panel has nothing to add: it folds away, and comes back with its conversation.
  useEffect(() => {
    if (!askButtonShown(host.view)) setOpen(false);
  }, [host.view]);

  const colony = askColony(override, host.colony);

  const patch = useCallback((id: number, change: (t: AskTurn) => AskTurn) => setTurns((list) => list.map((t) => (t.id === id ? change(t) : t))), []);

  const send = useCallback(
    async (question: string) => {
      const text = question.trim();
      if (!text) return;
      const id = next.current++;
      const about = askColony(override, hostRef.current.colony);
      setTurns((list) => [...list, { id, question: text, colony: about?.id ?? null, text: "", tools: [], phase: "thinking", error: null }]);
      setDraft("");
      const controller = new AbortController();
      abort.current = controller;
      try {
        let chatId = chatRef.current;
        if (!chatId) {
          const models = await api.chatModels();
          if (!models.default) throw new Error("No model is reachable yet. Add a provider under Settings → Model providers.");
          const meta = await api.createChat({ model: models.default, workspace: hostRef.current.org ?? undefined });
          chatId = meta.id;
          setChat(chatId);
        }
        await api.sendChat(
          chatId,
          { content: text, attachments: about ? [{ kind: "colony", id: about.id }] : [] },
          (event) => {
            if (event.type === "delta") patch(id, (t) => ({ ...t, phase: "streaming", text: t.text + event.text }));
            else if (event.type === "tool") {
              if (event.approval) addApproval(event.approval);
              patch(id, (t) => ({ ...t, tools: [...t.tools, event.note] }));
            } else if (event.type === "done") patch(id, (t) => ({ ...t, phase: "done" }));
            else if (event.type === "error") patch(id, (t) => ({ ...t, phase: "error", error: event.message }));
          },
          controller.signal,
        );
        patch(id, (t) => (t.phase === "error" ? t : { ...t, phase: "done" }));
      } catch (e) {
        if (!controller.signal.aborted) patch(id, (t) => ({ ...t, phase: "error", error: errorMessage(e) }));
      }
    },
    [api, override, patch, addApproval],
  );

  const stop = useCallback(() => abort.current?.abort(), []);
  const reset = useCallback(() => {
    abort.current?.abort();
    setTurns([]);
    setChat(null);
    setDraft("");
  }, []);

  const handle = useMemo<AskHandle>(() => ({ open, close, toggle, isOpen }), [open, close, toggle, isOpen]);
  const button = useRef<HTMLButtonElement>(null);

  return (
    <AskContext.Provider value={handle}>
      {children}
      {askButtonShown(host.view) && <AskButton ref={button} open={isOpen} onClick={toggle} />}
      {isOpen && askButtonShown(host.view) && (
        <AskPanel
          anchor={button}
          host={host}
          colony={colony}
          detached={override === null}
          onAttach={() => setOverride(host.colony ?? undefined)}
          onDetach={() => setOverride(null)}
          turns={turns}
          chat={chat}
          draft={draft}
          onDraft={setDraft}
          approvals={approvals}
          onSend={(q) => void send(q)}
          onStop={stop}
          onReset={reset}
          onClose={close}
        />
      )}
    </AskContext.Provider>
  );
}

/** The round chat bubble that opens the panel: bottom right on a desktop, above the tab bar on a phone. */
function AskButton({ ref, open, onClick }: { ref: Ref<HTMLButtonElement>; open: boolean; onClick: () => void }): ReactElement {
  return (
    <button
      ref={ref}
      type="button"
      onClick={onClick}
      aria-label="Ask Colonizer"
      aria-haspopup="dialog"
      aria-expanded={open}
      aria-keyshortcuts="Meta+J Control+J"
      title={`Ask Colonizer · ${MOD}J`}
      data-ask-button
      className={cx(
        "spot-fab fixed bottom-5 right-5 z-[60] grid size-12 cursor-pointer place-items-center rounded-full border-0 bg-accent text-on-accent transition-transform hover:scale-105 active:scale-95",
        // On a phone it stacks over the search button, above the tab bar.
        "max-sm:bottom-[calc(8.25rem+env(safe-area-inset-bottom))] max-sm:right-4",
        open && "ring-4 ring-accent/25",
      )}
    >
      <IconChat size={22} />
    </button>
  );
}

export function AskPanel({
  anchor,
  host,
  colony,
  detached,
  onAttach,
  onDetach,
  turns,
  chat,
  draft,
  onDraft,
  approvals,
  onSend,
  onStop,
  onReset,
  onClose,
}: {
  anchor: RefObject<HTMLElement | null>;
  host: SpotlightHost;
  colony: Session | null;
  detached: boolean;
  onAttach: () => void;
  onDetach: () => void;
  turns: AskTurn[];
  chat: string | null;
  draft: string;
  onDraft: (value: string) => void;
  approvals: ReturnType<typeof useApprovals>;
  onSend: (question: string) => void;
  onStop: () => void;
  onReset: () => void;
  onClose: () => void;
}): ReactElement {
  const busy = turns.at(-1)?.phase === "thinking" || turns.at(-1)?.phase === "streaming";
  const scroller = useRef<HTMLDivElement>(null);
  const last = turns.at(-1);
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [turns.length, last?.text, last?.tools.length]);

  const sections: PanelSection[] =
    turns.length === 0 && draft.trim() === ""
      ? [
          {
            id: "suggestions",
            title: "Try asking",
            rows: SUGGESTIONS.filter((s) => !s.colonyOnly || colony).map(
              (s): PanelRow => ({ id: s.id, title: s.text, subtitle: s.hint, leading: <Tile icon="ask" />, verb: "ask", onPick: () => onSend(s.text) }),
            ),
          },
        ]
      : [];
  const nav = usePanelNav(sections, draft, false);
  const listId = "ask-list";
  const open = chat ? () => (host.onOpenChat(chat), onClose()) : () => (host.onOpenChat(null), onClose());

  return (
    <PanelFrame label="Ask Colonizer" placement="anchored" side="above" anchor={anchor} align="end" width={420} onClose={onClose} testId="ask-panel" modeless>
      <div className="flex shrink-0 items-center gap-2 px-4 pb-0 pt-3">
        <span className="grid size-6 place-items-center text-accent">
          <AntGlyph size={22} />
        </span>
        <h2 className="m-0 min-w-0 flex-1 text-body-lg font-semibold">Ask Colonizer</h2>
        {turns.length > 0 && (
          <button type="button" onClick={onReset} className="cursor-pointer border-0 bg-transparent p-0 text-small text-muted hover:text-text">
            New chat
          </button>
        )}
      </div>
      <div className="flex shrink-0 flex-wrap items-center gap-1.5 px-4 pb-2.5 pt-2">
        {colony ? (
          <span className="spot-chip" data-on="true" data-ask-context="colony" title="This colony goes along with every question">
            <span className="max-w-[16rem] truncate font-mono">{askContextLabel(colony, host.org)}</span>
            <button type="button" aria-label="detach this colony" onClick={onDetach} className="cursor-pointer border-0 bg-transparent p-0 text-current opacity-70 hover:opacity-100">
              ×
            </button>
          </span>
        ) : (
          <>
            <span className="spot-chip cursor-default" data-ask-context="org" title="The workspace this chat looks at">
              {askContextLabel(null, host.org)}
            </span>
            {detached && host.colony && (
              <button type="button" className="spot-chip" onClick={onAttach}>
                + {askContextLabel(host.colony, host.org)}
              </button>
            )}
          </>
        )}
      </div>

      {turns.length > 0 ? (
        <div ref={scroller} aria-live="polite" aria-busy={busy} className="scroll-thin min-h-0 flex-1 overflow-y-auto border-t border-border px-4 py-3 sm:max-h-[min(46vh,420px)]">
          <div className="space-y-4">
            {turns.map((turn) => (
              <div key={turn.id} data-turn={turn.id}>
                <p className="m-0 mb-1.5 ml-auto w-fit max-w-[88%] rounded-2xl rounded-br-md bg-accent px-3 py-1.5 text-body-sm text-on-accent">{turn.question}</p>
                <div className="spot-answer text-body-sm">
                  {turn.text ? (
                    <ChatMarkdown text={turn.text} live={turn.phase === "streaming"} />
                  ) : turn.phase === "thinking" && turn.tools.length === 0 ? (
                    <span className="chat-dots inline-flex gap-1 py-1.5" aria-label="thinking">
                      <i />
                      <i />
                      <i />
                    </span>
                  ) : null}
                  {turn.tools.length > 0 && <ToolCalls notes={turn.tools} approvals={approvals} />}
                  {turn.error && (
                    <p role="alert" className="m-0 mt-1.5 rounded-lg border border-err/30 bg-err/5 px-2.5 py-1.5 text-small-lg text-err">
                      {turn.error}
                    </p>
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      ) : (
        <PanelList listId={listId} sections={sections} selectedId={nav.selectedId} onHover={nav.setPickedId} className="sm:max-h-[min(40vh,320px)]" />
      )}

      <div className="border-t border-border">
        <PanelInput
          value={draft}
          onChange={onDraft}
          placeholder={turns.length > 0 ? "Ask a follow-up…" : "Ask anything…"}
          label="Ask Colonizer"
          multiline
          icon={<IconChat size={18} />}
          onClose={onClose}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing) return;
            if (e.key === "Enter" && !e.shiftKey && draft.trim() !== "" && !busy) {
              e.preventDefault();
              onSend(draft);
              return;
            }
            if (turns.length === 0 && draft.trim() === "") nav.onKey(e);
          }}
          end={
            busy ? (
              <button type="button" onClick={onStop} className="spot-btn spot-btn-quiet self-center">
                Stop
              </button>
            ) : undefined
          }
        />
      </div>
      <PanelFooter
        hints={[
          { keys: ["↵"], label: "send" },
          { keys: ["⇧", "↵"], label: "new line" },
          { keys: [MOD, "J"], label: "toggle" },
        ]}
        end={
          <button type="button" onClick={open} className="cursor-pointer border-0 bg-transparent p-0 text-small font-medium text-text hover:text-accent">
            Open full chat ›
          </button>
        }
      />
    </PanelFrame>
  );
}
