// Chat: a direct conversation with a model — no colony, no microVM. Conversations live on the
// mothership (GET/POST /api/chat); the reply streams in as the model writes it. The model is any
// configured `<provider>/<model>`, or a plain Claude model when there is an Anthropic API key or an
// Anthropic provider — never the Claude subscription login, which the colonies run on.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import { errorMessage, useApi, useToast } from "../context";
import { ModelPicker } from "../components/ModelPicker";
import { Button, Spinner, cx, timeAgo } from "../components/ui";
import { formatCost, formatTokens } from "../spend";
import { useModels } from "../useModels";
import type { ChatMessage, ChatMeta, ChatModels, Repo, Session } from "../types";

/** Whether a model needs Claude access this install lacks, and the reason to show. Pure, for the tests. */
export function blockedReason(model: string, models: ChatModels | null): string | null {
  if (!model.trim()) return "Pick a model.";
  if (!models || model.includes("/")) return null;
  return models.claude.available ? null : models.claude.reason ?? "Claude models are not available here.";
}

/** A conversation as colony instructions: every turn, labelled, oldest first. Pure, for the tests. */
export function conversationAsInstructions(meta: ChatMeta | null, messages: readonly ChatMessage[], upTo?: string): string {
  const cut = upTo ? messages.findIndex((m) => m.id === upTo) : -1;
  const turns = cut >= 0 ? messages.slice(0, cut + 1) : messages;
  const body = turns
    .filter((m) => !m.error && m.content.trim())
    .map((m) => `${m.role === "user" ? "Me" : "Assistant"}:\n${m.content.trim()}`)
    .join("\n\n");
  return `Work on what this conversation${meta?.title ? ` ("${meta.title}")` : ""} settles on.\n\n${body}`;
}

function CodeBlock({ children }: { children?: ReactNode }): ReactElement {
  const ref = useRef<HTMLPreElement>(null);
  const toast = useToast();
  return (
    <div className="group relative">
      <pre ref={ref} className="scroll-thin overflow-x-auto rounded-lg border border-border bg-panel-2 p-3 font-mono text-[12.5px] leading-relaxed">
        {children}
      </pre>
      <button
        type="button"
        onClick={() => {
          void navigator.clipboard?.writeText(ref.current?.innerText ?? "").then(
            () => toast("Copied"),
            () => toast("Could not copy", "error"),
          );
        }}
        className="absolute right-2 top-2 cursor-pointer rounded-md border border-border bg-panel px-2 py-0.5 text-[11.5px] text-muted opacity-0 transition-opacity hover:text-text focus-visible:opacity-100 group-hover:opacity-100"
      >
        Copy
      </button>
    </div>
  );
}

function ChatMarkdown({ text }: { text: string }): ReactElement {
  return (
    <div className="md break-words text-[14px] leading-relaxed [overflow-wrap:anywhere]">
      <ReactMarkdown components={{ pre: ({ children }) => <CodeBlock>{children}</CodeBlock> }}>{text}</ReactMarkdown>
    </div>
  );
}

export function ChatView({
  org,
  repos,
  sessions,
  autopilotDefault,
  initialPrompt,
  onPromptTaken,
  onCreated,
}: {
  org: string | null;
  repos: readonly Repo[];
  sessions: readonly Session[];
  autopilotDefault: boolean;
  /** A question sent from the composer's Ask mode: a new conversation starts with it. */
  initialPrompt?: { text: string; n: number } | null;
  onPromptTaken?: () => void;
  onCreated: (session: Session) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const modelOptions = useModels();
  const [models, setModels] = useState<ChatModels | null>(null);
  const [chats, setChats] = useState<ChatMeta[]>([]);
  const [current, setCurrent] = useState<ChatMeta | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [streaming, setStreaming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [colony, setColony] = useState<string>("");
  // An attached repository file ("owner/repo" + path), read from the mothership's clone.
  const [fileRepo, setFileRepo] = useState<string>("");
  const [filePath, setFilePath] = useState<string>("");
  const [handoff, setHandoff] = useState<{ instructions: string; repo: string } | null>(null);
  const abort = useRef<AbortController | null>(null);
  const bottom = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => {
    const list = await api.chats().catch(() => ({ chats: [] as ChatMeta[] }));
    setChats(list.chats);
    return list.chats;
  }, [api]);

  useEffect(() => {
    void api.chatModels().then(setModels, () => setModels(null));
    void refresh();
  }, [api, refresh]);

  const open = useCallback(
    async (id: string) => {
      abort.current?.abort();
      const data = await api.chat(id);
      setCurrent(data.chat);
      setMessages(data.messages);
      setStreaming(null);
    },
    [api],
  );

  useEffect(() => {
    bottom.current?.scrollIntoView({ block: "end" });
  }, [messages, streaming]);

  const newChat = useCallback(async () => {
    const meta = await api.createChat({ model: models?.default ?? "", workspace: org ?? undefined });
    setCurrent(meta);
    setMessages([]);
    setStreaming(null);
    void refresh();
    return meta;
  }, [api, models, org, refresh]);

  const send = useCallback(
    async (meta: ChatMeta, content: string | null) => {
      const controller = new AbortController();
      abort.current = controller;
      setBusy(true);
      setStreaming("");
      if (content) {
        setMessages((m) => [...m, { id: `local-${Date.now()}`, role: "user", content, ts: new Date().toISOString(), input_tokens: 0, output_tokens: 0, stopped: false }]);
      } else {
        setMessages((m) => {
          const next = [...m];
          while (next.at(-1)?.role === "assistant") next.pop();
          return next;
        });
      }
      try {
        await api.sendChat(
          meta.id,
          content
            ? {
                content,
                context:
                  colony || (fileRepo && filePath.trim())
                    ? { colony: colony || undefined, file: fileRepo && filePath.trim() ? { repo: fileRepo, path: filePath.trim() } : undefined }
                    : undefined,
              }
            : { regenerate: true },
          (event) => {
            if (event.type === "delta") setStreaming((s) => (s ?? "") + event.text);
            else if (event.type === "done") {
              setMessages((m) => [...m, event.message]);
              setStreaming(null);
            } else {
              if (event.message_record) setMessages((m) => [...m, event.message_record as ChatMessage]);
              setStreaming(null);
              toast(event.message, "error");
            }
          },
          controller.signal,
        );
      } catch (error) {
        if (!controller.signal.aborted) toast(errorMessage(error), "error");
      } finally {
        setBusy(false);
        abort.current = null;
        // The stored record is the source of truth (a stopped reply is kept, marked stopped).
        void api.chat(meta.id).then((d) => {
          setMessages(d.messages);
          setCurrent(d.chat);
          setStreaming(null);
        }, () => {});
        void refresh();
      }
    },
    [api, colony, fileRepo, filePath, refresh, toast],
  );

  // A question from the composer's Ask mode starts a new conversation.
  const taken = useRef(0);
  useEffect(() => {
    if (!initialPrompt || initialPrompt.n === taken.current || !models) return;
    taken.current = initialPrompt.n;
    onPromptTaken?.();
    void (async () => {
      const meta = await newChat();
      await send(meta, initialPrompt.text);
    })();
  }, [initialPrompt, models, newChat, send, onPromptTaken]);

  const model = current?.model ?? models?.default ?? "";
  const blocked = blockedReason(model, models);
  const submit = async () => {
    const text = draft.trim();
    if (!text || busy || blocked) return;
    setDraft("");
    const meta = current ?? (await newChat());
    await send(meta, text);
  };
  const cost = useMemo(() => messages.reduce((n, m) => n + (m.cost_usd ?? 0), 0), [messages]);
  const tokens = useMemo(() => messages.reduce((n, m) => n + m.input_tokens + m.output_tokens, 0), [messages]);
  const liveColonies = sessions.filter((s) => !["merged", "closed", "no_changes", "stopped", "failed"].includes(s.status));

  return (
    <main className="cockpit flex min-h-0 flex-1">
      <aside aria-label="conversations" className="flex w-[260px] shrink-0 flex-col border-r border-border">
        <div className="flex items-center gap-2 px-3 py-3">
          <h1 className="m-0 flex-1 text-[18px] font-semibold">Chat</h1>
          <Button onClick={() => void newChat()}>New</Button>
        </div>
        <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-1.5 pb-3">
          {chats.length === 0 && <p className="px-2 py-2 text-[12.5px] text-faint">No conversations yet.</p>}
          {chats.map((c) => (
            <div key={c.id} className={cx("group flex items-center gap-1 rounded-md", current?.id === c.id ? "bg-panel-2" : "hover:bg-panel-2")}>
              <button type="button" onClick={() => void open(c.id)} className="min-w-0 flex-1 cursor-pointer border-0 bg-transparent px-2 py-1.5 text-left">
                <div className="truncate text-[13px] text-text">{c.title || "New conversation"}</div>
                <div className="truncate text-[11px] text-faint">
                  {c.model} · {timeAgo(c.updated_at)}
                </div>
              </button>
              <button
                type="button"
                aria-label={`rename ${c.title || "conversation"}`}
                onClick={() => {
                  const title = window.prompt("Rename conversation", c.title);
                  if (title !== null) void api.patchChat(c.id, { title }).then(() => refresh());
                }}
                className="cursor-pointer rounded border-0 bg-transparent px-1 text-[12px] text-faint opacity-0 hover:text-text group-hover:opacity-100"
              >
                ✎
              </button>
              <button
                type="button"
                aria-label={`delete ${c.title || "conversation"}`}
                onClick={() => {
                  void api.deleteChat(c.id).then(() => {
                    if (current?.id === c.id) {
                      setCurrent(null);
                      setMessages([]);
                    }
                    void refresh();
                  });
                }}
                className="mr-1 cursor-pointer rounded border-0 bg-transparent px-1 text-[12px] text-faint opacity-0 hover:text-err group-hover:opacity-100"
              >
                ×
              </button>
            </div>
          ))}
        </div>
      </aside>

      <section aria-label="conversation" className="flex min-w-0 flex-1 flex-col">
        <div className="flex flex-wrap items-center gap-3 border-b border-border px-5 py-2.5">
          <div className="min-w-0 flex-1 truncate text-[14px] font-medium">{current?.title || "New conversation"}</div>
          <div className="w-[340px] max-w-full">
            <ModelPicker
              value={model}
              models={modelOptions}
              ariaLabel="chat model"
              emptyLabel={models?.default ? `Default · ${models.default}` : "Default"}
              onChange={(value) => {
                if (current) void api.patchChat(current.id, { model: value }).then(setCurrent);
                else void newChat().then((meta) => api.patchChat(meta.id, { model: value }).then(setCurrent));
              }}
            />
          </div>
          <label className="flex items-center gap-1.5 text-[12px] text-muted">
            Context
            <select value={colony} onChange={(e) => setColony(e.target.value)} className="rounded-md border border-border bg-transparent px-1.5 py-0.5 text-[12px] text-text">
              <option value="">none</option>
              {liveColonies.map((s) => (
                <option key={s.id} value={s.id}>
                  {(s.summary || s.issue_title || s.id).slice(0, 48)}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-1.5 text-[12px] text-muted">
            File
            <select value={fileRepo} onChange={(e) => setFileRepo(e.target.value)} aria-label="file's repository" className="max-w-[160px] rounded-md border border-border bg-transparent px-1.5 py-0.5 text-[12px] text-text">
              <option value="">none</option>
              {repos
                .filter((r) => !org || r.full_name.startsWith(`${org}/`))
                .map((r) => (
                  <option key={r.full_name} value={r.full_name}>
                    {r.full_name}
                  </option>
                ))}
            </select>
            {fileRepo && (
              <input
                value={filePath}
                onChange={(e) => setFilePath(e.target.value)}
                placeholder="path/in/repo.ts"
                aria-label="file path"
                className="w-[180px] rounded-md border border-border bg-transparent px-1.5 py-0.5 font-mono text-[12px] text-text outline-none placeholder:text-faint"
              />
            )}
          </label>
          <span className="text-[11.5px] tabular-nums text-faint">
            {formatTokens(tokens)} tokens{cost > 0 ? ` · ${formatCost(cost)}` : ""}
          </span>
        </div>
        {blocked && <div className="border-b border-border bg-warn/10 px-5 py-2 text-[12.5px] text-warn">{blocked}</div>}

        <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-5">
          <div className="mx-auto flex max-w-[820px] flex-col gap-5">
            {messages.length === 0 && streaming === null && (
              <p className="py-10 text-center text-[13.5px] text-faint">Ask anything. The conversation stays on this mothership; only the model you pick sees it.</p>
            )}
            {messages.map((m, i) => (
              <div key={m.id} className={cx("flex flex-col gap-1", m.role === "user" && "items-end")}>
                <div
                  className={cx(
                    "max-w-full rounded-xl px-3.5 py-2.5",
                    m.role === "user" ? "bg-accent-soft text-text" : "bg-transparent",
                    m.error && "border border-err/40",
                  )}
                >
                  {m.role === "user" ? <p className="m-0 whitespace-pre-wrap text-[14px]">{m.content}</p> : <ChatMarkdown text={m.content || (m.error ? "" : "…")} />}
                  {m.error && <p className="m-0 mt-1 text-[12.5px] text-err">{m.error}</p>}
                </div>
                {m.role === "assistant" && (
                  <div className="flex items-center gap-3 px-1 text-[11.5px] text-faint">
                    <span>
                      {m.model} · {formatTokens(m.input_tokens + m.output_tokens)} tokens{m.cost_usd != null ? ` · ${formatCost(m.cost_usd)}` : ""}
                      {m.stopped ? " · stopped" : ""}
                    </span>
                    {i === messages.length - 1 && !busy && (
                      <button type="button" onClick={() => current && void send(current, null)} className="cursor-pointer border-0 bg-transparent p-0 text-faint hover:text-text">
                        Regenerate
                      </button>
                    )}
                    <button
                      type="button"
                      onClick={() => setHandoff({ instructions: conversationAsInstructions(current, messages, m.id), repo: repos.find((r) => !org || r.full_name.startsWith(`${org}/`))?.full_name ?? "" })}
                      className="cursor-pointer border-0 bg-transparent p-0 text-faint hover:text-text"
                    >
                      Turn into a colony
                    </button>
                  </div>
                )}
              </div>
            ))}
            {streaming !== null && (
              <div className="flex flex-col gap-1">
                {streaming ? <ChatMarkdown text={streaming} /> : <Spinner />}
              </div>
            )}
            <div ref={bottom} />
          </div>
        </div>

        <div className="border-t border-border px-5 py-3">
          <div className="mx-auto flex max-w-[820px] items-end gap-2">
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void submit();
                }
              }}
              rows={2}
              placeholder={blocked ? "Pick a model you can reach first…" : "Ask anything — ⏎ to send, ⇧⏎ for a new line"}
              aria-label="message"
              className="min-h-[44px] flex-1 resize-y rounded-xl border border-border bg-panel px-3 py-2 text-[14px] text-text outline-none placeholder:text-faint focus:border-border-strong"
            />
            {busy ? (
              <Button onClick={() => abort.current?.abort()}>Stop</Button>
            ) : (
              <Button variant="primary" disabled={!draft.trim() || Boolean(blocked)} onClick={() => void submit()}>
                Send
              </Button>
            )}
          </div>
        </div>
      </section>

      {handoff && (
        <HandoffDialog
          repos={repos}
          org={org}
          initial={handoff}
          onClose={() => setHandoff(null)}
          onLaunch={async (repo, instructions) => {
            const session = await api.createSession({ repo, instructions, autopilot: autopilotDefault });
            toast(`Colony launched on ${repo}`);
            setHandoff(null);
            onCreated(session);
          }}
        />
      )}
    </main>
  );
}

/** The conversation as a colony: pick the repository, edit the prefilled instructions, launch. */
function HandoffDialog({
  repos,
  org,
  initial,
  onClose,
  onLaunch,
}: {
  repos: readonly Repo[];
  org: string | null;
  initial: { instructions: string; repo: string };
  onClose: () => void;
  onLaunch: (repo: string, instructions: string) => Promise<void>;
}): ReactElement {
  const ref = useRef<HTMLDialogElement>(null);
  const toast = useToast();
  const [repo, setRepo] = useState(initial.repo);
  const [instructions, setInstructions] = useState(initial.instructions);
  const [launching, setLaunching] = useState(false);
  useEffect(() => {
    if (ref.current && !ref.current.open) ref.current.showModal();
  }, []);
  const scoped = repos.filter((r) => !org || r.full_name.startsWith(`${org}/`));
  return (
    <dialog ref={ref} onClose={onClose} aria-label="turn into a colony" className="m-auto w-[min(720px,calc(100vw-24px))] rounded-2xl border border-border bg-panel p-0 text-text backdrop:bg-black/50">
      <div className="flex flex-col gap-3 p-5">
        <h2 className="m-0 text-[16px] font-semibold">Turn into a colony</h2>
        <label className="flex flex-col gap-1 text-[12.5px] text-muted">
          Repository
          <select value={repo} onChange={(e) => setRepo(e.target.value)} className="rounded-md border border-border bg-transparent px-2 py-1.5 font-mono text-[13px] text-text">
            <option value="">choose…</option>
            {scoped.map((r) => (
              <option key={r.full_name} value={r.full_name}>
                {r.full_name}
              </option>
            ))}
          </select>
        </label>
        <label className="flex flex-col gap-1 text-[12.5px] text-muted">
          Instructions
          <textarea value={instructions} onChange={(e) => setInstructions(e.target.value)} rows={12} className="scroll-thin rounded-md border border-border bg-transparent px-2 py-1.5 font-mono text-[12.5px] text-text" />
        </label>
        <div className="flex justify-end gap-2">
          <Button onClick={() => ref.current?.close()}>Cancel</Button>
          <Button
            variant="primary"
            disabled={!repo || !instructions.trim() || launching}
            onClick={() => {
              setLaunching(true);
              onLaunch(repo, instructions.trim())
                .catch((e) => toast(errorMessage(e), "error"))
                .finally(() => setLaunching(false));
            }}
          >
            {launching && <Spinner />} Launch colony
          </Button>
        </div>
      </div>
    </dialog>
  );
}
