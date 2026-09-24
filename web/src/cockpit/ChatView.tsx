// Chat: a direct conversation with a model — no colony, no microVM. Conversations live on the
// mothership (GET/POST /api/chat); replies stream in as the model writes them. The model is any
// configured `<provider>/<model>`, or a plain Claude model when there is an Anthropic API key or an
// Anthropic provider — never the Claude subscription login, which the colonies run on.
//
// The view: a conversation list (search, workspace filter, pinned and dated groups), an empty-state
// hero with Colonizer-aware suggestions, the conversation with per-message actions, and a composer
// that attaches files, colonies, maps, issues, snippets and images, compares two models, and runs
// slash commands. The pieces live in ./chat/.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { LogoMark } from "../components/Sidebar";
import { IconBranch, IconDownload, IconSearch, IconSliders, IconX } from "../components/icons";
import { Button, cx, orgOf, stored, store } from "../components/ui";
import { formatCost, formatTokens } from "../spend";
import { useModels } from "../useModels";
import type { ChatAttachment, ChatAttachmentNote, ChatMessage, ChatMeta, ChatModels, ChatPatch, ChatPrefs, ChatStreamEvent, Repo, Session } from "../types";
import { AttachMenu, pending, type AttachStep, type Pending } from "./chat/AttachMenu";
import { ChatComposer, MOD, type SendKey } from "./chat/ChatComposer";
import { ChatSidebar } from "./chat/ChatSidebar";
import { HandoffDialog, ImageLightbox, IssueDialog, LoopDialog } from "./chat/Dialogs";
import { MessageRow, StreamingRow, type MessageAction, type OpenImage } from "./chat/Message";
import { Popover } from "./chat/Popover";
import { PersonaPicker } from "./chat/PersonaPicker";
import {
  candidatesByParent,
  estimateTokens,
  hasImages,
  imageProblem,
  inputCost,
  LEGACY_FEEDBACK_KEY,
  LEGACY_PERSONA_KEY,
  legacyEntries,
  loadPersonas,
  personaEdit,
  personaFor,
  pricingOf,
  searchMessages,
  storedImages,
  visionCapable,
  type Persona,
  type SlashCommand,
} from "./chat/logic";

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
    .filter((m) => !m.error && !m.candidate && m.content.trim())
    .map((m) => {
      // A colony cannot see the chat's images; it is told they were there.
      const images = storedImages(m.attachments).map((i) => `[image shared in the chat, not attached: ${i.label}]\n`);
      return `${m.role === "user" ? "Me" : "Assistant"}:\n${images.join("")}${m.content.trim()}`;
    })
    .join("\n\n");
  return `Work on what this conversation${meta?.title ? ` ("${meta.title}")` : ""} settles on.\n\n${body}`;
}

export interface Suggestion {
  id: string;
  title: string;
  detail: string;
  prompt: string;
  attach?: ChatAttachment;
  /** Opens the attach menu at this step instead of attaching directly. */
  step?: AttachStep;
}

/** The suggestion cards on an empty conversation, from what this workspace has going on. Pure, for the tests. */
export function suggestionsFor(org: string | null, repos: readonly Repo[], sessions: readonly Session[]): Suggestion[] {
  const inOrg = (repo: string) => !org || repo.toLowerCase().startsWith(`${org.toLowerCase()}/`);
  const mine = sessions.filter((s) => inOrg(s.repo)).sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  const repo =
    mine[0]?.repo ??
    repos
      .filter((r) => !r.archived && inOrg(r.full_name))
      .sort((a, b) => (b.pushed_at ?? "").localeCompare(a.pushed_at ?? ""))[0]?.full_name;
  const failed = mine.find((s) => s.status === "failed");
  const out: Suggestion[] = [];
  if (repo)
    out.push({
      id: "architecture",
      title: `Explain ${repo.split("/")[1]}'s architecture`,
      detail: "From its stored architecture map",
      prompt: `Explain ${repo}'s architecture: the main components, how they talk to each other, and where I should start reading.`,
      attach: { kind: "map", repo },
    });
  out.push({
    id: "today",
    title: "What did my colonies do today?",
    detail: org ? `Everything that moved in ${org}` : "Everything that moved in 24 h",
    prompt: "What did my colonies do today? Group it by repository, call out failures, and list anything waiting on me.",
    attach: { kind: "colonies_today", org: org ?? undefined },
  });
  if (failed)
    out.push({
      id: "failed",
      title: `Why did “${(failed.summary || failed.issue_title || failed.id).slice(0, 40)}” fail?`,
      detail: failed.repo,
      prompt: "Why did this colony fail, and what should I change before retrying it?",
      attach: { kind: "colony", id: failed.id },
    });
  out.push({
    id: "release",
    title: "Draft release notes",
    detail: "From pull requests merged this week",
    prompt: "Draft release notes from these merged pull requests: group them under Added, Changed and Fixed, one line each with the PR link.",
    attach: { kind: "merged_prs", org: org ?? undefined, days: 7 },
  });
  out.push({ id: "review", title: "Review a file", detail: "Pick one from a repository", prompt: "Review this file: bugs and security problems first, then missing tests, then anything confusing.", step: "file-repo" });
  out.push({
    id: "plan",
    title: "Plan an issue into colony tasks",
    detail: "Pick an open GitHub issue",
    prompt: "Plan this issue into colony-sized tasks. For each: the goal, the files it likely touches, and how to check it is done.",
    step: "issue-repo",
  });
  return out;
}

/** Settings for a conversation not created yet: it is created with them on the first send. */
interface Draft {
  model: string;
  system: string;
  persona: string;
  temperature: number | null;
  max_tokens: number;
}

/** A reply streaming in; `done` once its lane has finished (compare waits for both). */
interface Live {
  model: string;
  text: string;
  done: boolean;
}

const FALLBACK_TITLE = "New conversation";
const read = (key: string) => stored(key);

export function ChatView({
  org,
  repos,
  sessions,
  autopilotDefault,
  initialPrompt,
  onPromptTaken,
  onCreated,
  workspaces = [],
  onOpenFile,
}: {
  org: string | null;
  repos: readonly Repo[];
  sessions: readonly Session[];
  autopilotDefault: boolean;
  /** A question sent from the composer's Ask mode: a new conversation starts with it. */
  initialPrompt?: { text: string; n: number } | null;
  onPromptTaken?: () => void;
  onCreated: (session: Session) => void;
  /** The workspaces and their logos, for the sidebar filter and pickers. */
  workspaces?: readonly { org: string; avatar: string | null }[];
  /** Opens a repository file in the Code page. */
  onOpenFile?: (repo: string, path: string) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const modelOptions = useModels();
  const claudeIds = useMemo(() => modelOptions.filter((m) => m.provider === "anthropic").map((m) => ({ id: m.id, label: m.id })), [modelOptions]);
  const avatarFor = useCallback((o: string) => workspaces.find((w) => w.org.toLowerCase() === o.toLowerCase())?.avatar ?? null, [workspaces]);

  const [models, setModels] = useState<ChatModels | null>(null);
  const [chats, setChats] = useState<ChatMeta[]>([]);
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [current, setCurrent] = useState<ChatMeta | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [attachments, setAttachments] = useState<Pending[]>([]);
  const [live, setLive] = useState<Live[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [compareModel, setCompareModel] = useState<string | null>(null);
  const [settings, setSettings] = useState<Draft>({ model: "", system: "", persona: "Plain", temperature: null, max_tokens: 4096 });
  const [prefs, setPrefs] = useState<ChatPrefs>({ personas: {}, feedback: {} });
  const personas = useMemo<Persona[]>(() => loadPersonas(prefs.personas), [prefs.personas]);
  const [lightbox, setLightbox] = useState<OpenImage | null>(null);
  const [sendKey, setSendKey] = useState<SendKey>(() => (read("colonizer.chat.sendKey") === "mod-enter" ? "mod-enter" : "enter"));
  const [collapsed, setCollapsed] = useState(() => read("colonizer.chat.sidebar") === "collapsed");
  const [search, setSearch] = useState<{ query: string; index: number } | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [renamingTitle, setRenamingTitle] = useState<string | null>(null);
  const [handoff, setHandoff] = useState<{ instructions: string; repo: string } | null>(null);
  const [loop, setLoop] = useState<{ name: string; prompt: string; repo: string } | null>(null);
  const [issue, setIssue] = useState<{ title: string; body: string; repo: string } | null>(null);

  const abort = useRef<AbortController | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const attachControl = useRef<((step: AttachStep) => void) | null>(null);
  const modelOpen = useRef<(() => void) | null>(null);
  const advancedButton = useRef<HTMLButtonElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const deletes = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const defaultRepo = useMemo(
    () =>
      sessions.find((s) => !org || orgOf(s).toLowerCase() === org.toLowerCase())?.repo ??
      repos.find((r) => !org || r.full_name.toLowerCase().startsWith(`${org.toLowerCase()}/`))?.full_name ??
      "",
    [sessions, repos, org],
  );

  const refresh = useCallback(async () => {
    const list = await api.chats().catch(() => ({ chats: [] as ChatMeta[] }));
    setChats(list.chats);
    return list.chats;
  }, [api]);

  useEffect(() => {
    void api.chatModels().then(
      (m) => {
        setModels(m);
        setSettings((s) => (s.model ? s : { ...s, model: m.default ?? "" }));
      },
      () => setModels(null),
    );
    void refresh();
  }, [api, refresh]);

  // Persona edits and reply notes live on the mothership; ones an earlier version kept in this
  // browser are moved up once.
  useEffect(() => {
    void (async () => {
      let p = await api.chatPrefs().catch(() => null);
      if (!p) return;
      try {
        for (const [id, system] of legacyEntries(read(LEGACY_PERSONA_KEY), p.personas)) p = await api.saveChatPersona(id, system);
        for (const [id, note] of legacyEntries(read(LEGACY_FEEDBACK_KEY), p.feedback)) p = await api.saveChatFeedback(id, note);
        store(LEGACY_PERSONA_KEY, null);
        store(LEGACY_FEEDBACK_KEY, null);
      } catch {
        /* kept in the browser; tried again next time */
      }
      setPrefs(p);
    })();
  }, [api]);

  // A deletion still inside its undo window happens now if the view goes away.
  useEffect(() => {
    const pendingDeletes = deletes.current;
    return () => {
      for (const [id, timer] of pendingDeletes) {
        clearTimeout(timer);
        void api.deleteChat(id).catch(() => {});
      }
    };
  }, [api]);

  const reset = useCallback(() => {
    abort.current?.abort();
    setCurrent(null);
    setMessages([]);
    setLive(null);
    setSearch(null);
    setCompareModel(null);
    setDraft("");
    setAttachments([]);
    setSettings((s) => ({ ...s, model: models?.default ?? s.model }));
    setTimeout(() => textarea.current?.focus(), 0);
  }, [models]);

  const open = useCallback(
    async (id: string) => {
      abort.current?.abort();
      try {
        const data = await api.chat(id);
        setCurrent(data.chat);
        setMessages(data.messages);
        setLive(null);
        setSearch(null);
        stick.current = true;
        setTimeout(() => scroller.current?.scrollTo({ top: scroller.current.scrollHeight }), 0);
      } catch (e) {
        toast(errorMessage(e), "error");
      }
    },
    [api, toast],
  );

  // Follow the reply while the reader is at the bottom; leave them be when they scroll up.
  useEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [messages, live]);

  const ensureChat = useCallback(async (): Promise<ChatMeta> => {
    if (current) return current;
    const meta = await api.createChat({
      model: settings.model || models?.default || "",
      system: settings.system || undefined,
      persona: settings.persona,
      temperature: settings.temperature ?? undefined,
      max_tokens: settings.max_tokens,
      workspace: org ?? undefined,
    });
    setCurrent(meta);
    setChats((cs) => [meta, ...cs]);
    return meta;
  }, [api, current, settings, models, org]);

  const onEvent = useCallback(
    (event: ChatStreamEvent) => {
      const lane = event.lane ?? 0;
      const finish = () => setLive((l) => l && l.map((x, i) => (i === lane ? { ...x, done: true } : x)));
      if (event.type === "delta") {
        setLive((l) => l && l.map((x, i) => (i === lane ? { ...x, text: x.text + event.text } : x)));
      } else if (event.type === "done") {
        setMessages((m) => [...m, event.message]);
        finish();
        if (event.chat) {
          const chat = event.chat;
          setCurrent((c) => (c?.id === chat.id ? chat : c));
          setChats((cs) => cs.map((c) => (c.id === chat.id ? chat : c)));
        }
      } else {
        if (event.message_record) setMessages((m) => [...m, event.message_record as ChatMessage]);
        finish();
        toast(event.message, "error");
      }
    },
    [toast],
  );

  const settle = useCallback(
    async (id: string) => {
      setBusy(false);
      abort.current = null;
      // The stored record is the source of truth (a stopped reply is kept, marked stopped).
      try {
        const d = await api.chat(id);
        setMessages(d.messages);
        setCurrent((c) => (c?.id === id ? d.chat : c));
      } catch {
        /* the conversation went away */
      }
      setLive(null);
      void refresh();
    },
    [api, refresh],
  );

  const optimistic = (content: string, attached: Pending[]): ChatMessage => ({
    id: `local-${Date.now()}`,
    role: "user",
    content,
    ts: new Date().toISOString(),
    input_tokens: 0,
    output_tokens: 0,
    stopped: false,
    attachments: attached.map((p): ChatAttachmentNote => (p.attachment.kind === "image" && "sha" in p.attachment ? { kind: "image", label: p.label, sha: p.attachment.sha } : { kind: p.attachment.kind, label: p.label })),
  });

  const send = useCallback(
    async (meta: ChatMeta, content: string | null, opts: { model?: string; attached?: Pending[] } = {}) => {
      const controller = new AbortController();
      abort.current = controller;
      stick.current = true;
      setBusy(true);
      const attached = opts.attached ?? [];
      setLive([{ model: opts.model ?? meta.model, text: "", done: false }]);
      if (content) setMessages((m) => [...m, optimistic(content, attached)]);
      else
        setMessages((m) => {
          const next = [...m];
          while (next.at(-1)?.role === "assistant") next.pop();
          return next;
        });
      try {
        await api.sendChat(
          meta.id,
          content ? { content, model: opts.model, attachments: attached.map((p) => p.attachment) } : { regenerate: true, model: opts.model },
          onEvent,
          controller.signal,
        );
      } catch (error) {
        if (!controller.signal.aborted) {
          toast(errorMessage(error), "error");
          // Refused before the model saw it: give the attachments back to fix and resend.
          if (attached.length) setAttachments((a) => (a.length ? a : attached));
        }
      } finally {
        await settle(meta.id);
      }
    },
    [api, onEvent, settle, toast],
  );

  const compare = useCallback(
    async (meta: ChatMeta, content: string, second: string, attached: Pending[]) => {
      const controller = new AbortController();
      abort.current = controller;
      stick.current = true;
      setBusy(true);
      setLive([
        { model: meta.model, text: "", done: false },
        { model: second, text: "", done: false },
      ]);
      setMessages((m) => [...m, optimistic(content, attached)]);
      try {
        await api.compareChat(meta.id, { content, models: [meta.model, second], attachments: attached.map((p) => p.attachment) }, onEvent, controller.signal);
      } catch (error) {
        if (!controller.signal.aborted) toast(errorMessage(error), "error");
      } finally {
        await settle(meta.id);
      }
    },
    [api, onEvent, settle, toast],
  );

  // A question from the composer's Ask mode starts a new conversation.
  const taken = useRef(0);
  useEffect(() => {
    if (!initialPrompt || initialPrompt.n === taken.current || !models) return;
    taken.current = initialPrompt.n;
    onPromptTaken?.();
    void (async () => {
      reset();
      const meta = await api.createChat({ model: models.default ?? "", workspace: org ?? undefined });
      setCurrent(meta);
      setMessages([]);
      void refresh();
      await send(meta, initialPrompt.text);
    })();
  }, [initialPrompt, models, onPromptTaken, api, org, refresh, reset, send]);

  const model = current?.model || settings.model || models?.default || "";
  const comparing = compareModel !== null;
  const blocked =
    blockedReason(model, models) ?? (comparing ? (compareModel ? blockedReason(compareModel, models) : "Pick the second model to compare with.") : null);
  const vision = visionCapable(model, models) && (!compareModel || visionCapable(compareModel, models));
  const uploading = attachments.some((p) => p.progress !== undefined);

  const submit = async () => {
    const text = draft.trim();
    if (!text || busy || blocked || uploading) return;
    const attached = attachments;
    setDraft("");
    setAttachments([]);
    try {
      const meta = await ensureChat();
      if (comparing && compareModel) await compare(meta, text, compareModel, attached);
      else await send(meta, text, { attached });
    } catch (e) {
      toast(errorMessage(e), "error");
      setDraft(text);
      setAttachments(attached);
    }
  };

  const upsertMeta = useCallback((m: ChatMeta) => {
    setChats((cs) => cs.map((c) => (c.id === m.id ? m : c)));
    setCurrent((c) => (c?.id === m.id ? m : c));
  }, []);

  const patch = useCallback(
    (body: ChatPatch) => {
      if (!current) {
        setSettings((s) => ({
          ...s,
          ...(body.model !== undefined ? { model: body.model } : {}),
          ...(body.system !== undefined ? { system: body.system ?? "" } : {}),
          ...(body.persona !== undefined ? { persona: body.persona ?? "Plain" } : {}),
          ...(body.max_tokens !== undefined ? { max_tokens: body.max_tokens } : {}),
          ...(body.temperature !== undefined ? { temperature: body.temperature < 0 ? null : body.temperature } : {}),
        }));
        return;
      }
      void api.patchChat(current.id, body).then(upsertMeta, (e) => toast(errorMessage(e), "error"));
    },
    [api, current, toast, upsertMeta],
  );

  const setModel = (value: string) => {
    patch({ model: value });
    if (!visionCapable(value, models) && attachments.some((a) => a.attachment.kind === "image"))
      toast({ title: "This model cannot see images", body: "Remove the attached images or pick a Claude or Anthropic-wire model.", kind: "warn" });
  };

  const addAttachment = useCallback((p: Pending) => {
    setAttachments((list) => (list.some((x) => x.key === p.key) ? list.map((x) => (x.key === p.key ? p : x)) : [...list, p]));
    setTimeout(() => textarea.current?.focus(), 0);
  }, []);

  const addImage = useCallback(
    (file: File) => {
      if (!vision) {
        toast({ title: "This model cannot see images", body: "Images go only to Claude models and Anthropic-wire providers. Switch models to attach one.", kind: "warn" });
        return;
      }
      const problem = imageProblem(file, attachments.filter((a) => a.attachment.kind === "image").length);
      if (problem) {
        toast(problem, "error");
        return;
      }
      // The image is stored on the mothership right away; the message then refers to it by hash.
      const name = file.name || "pasted image";
      const preview = URL.createObjectURL(file);
      const p: Pending = { ...pending({ kind: "image", sha: "", name }, 1600 * 4, name, preview), progress: 0 };
      addAttachment(p);
      const update = (f: (x: Pending) => Pending) => setAttachments((list) => list.map((x) => (x.key === p.key ? f(x) : x)));
      api.uploadChatImage(file, (fraction) => update((x) => ({ ...x, progress: Math.min(fraction, 0.99) }))).then(
        (ref) => update((x) => ({ ...x, attachment: { kind: "image", sha: ref.sha, name }, progress: undefined })),
        (e) => {
          toast(`${name}: ${errorMessage(e)}`, "error");
          setAttachments((list) => list.filter((x) => x.key !== p.key));
          URL.revokeObjectURL(preview);
        },
      );
    },
    [api, vision, attachments, toast, addAttachment],
  );

  const estimate = useMemo(() => {
    if (attachments.length === 0 && draft.length < 2000) return null;
    const chars = attachments.reduce((n, p) => n + (p.chars ?? 0), 0) + draft.length;
    const tokens = estimateTokens(chars);
    return { tokens, cost: inputCost(tokens, pricingOf(model, models)), unknown: attachments.some((p) => p.chars === null) };
  }, [attachments, draft, model, models]);

  const loopFrom = useCallback(
    (upTo?: string) => current && setLoop({ name: (current.title || "Chat loop").slice(0, 60), prompt: conversationAsInstructions(current, messages, upTo), repo: defaultRepo }),
    [current, messages, defaultRepo],
  );

  const onSlash = (cmd: SlashCommand) => {
    if (cmd === "colony") attachControl.current?.("colony");
    else if (cmd === "file") attachControl.current?.("file-repo");
    else if (cmd === "model") modelOpen.current?.();
    else if (cmd === "system") setAdvancedOpen(true);
    else if (cmd === "clear") reset();
    else if (!current || messages.length === 0) toast("Have a conversation first; the loop is made from it.", "info");
    else loopFrom();
  };

  const onAction = useCallback(
    async (m: ChatMessage, action: MessageAction) => {
      if (!current) return;
      try {
        switch (action.kind) {
          case "regenerate":
            await send(current, null, { model: action.model });
            break;
          case "fork": {
            const meta = await api.forkChat(current.id, m.id, true);
            await refresh();
            await open(meta.id);
            toast({ title: "Branched", body: "A copy of the conversation up to that message; the original is unchanged.", kind: "success" });
            break;
          }
          case "edit": {
            const meta = await api.forkChat(current.id, m.id, false);
            await refresh();
            await open(meta.id);
            // The edited message keeps its images: they are stored, so they go again by reference.
            const images = storedImages(m.attachments).map((i) => pending({ kind: "image", sha: i.sha, name: i.label }, 1600 * 4, i.label));
            await send(meta, action.content, { attached: images });
            break;
          }
          case "note":
            setPrefs(await api.saveChatFeedback(m.id, action.note));
            break;
          case "pick":
            setMessages((await api.pickChat(current.id, m.id)).messages);
            break;
          case "colony":
            setHandoff({ instructions: conversationAsInstructions(current, messages, m.id), repo: defaultRepo });
            break;
          case "loop":
            loopFrom(m.id);
            break;
          case "issue": {
            const at = messages.findIndex((x) => x.id === m.id);
            const question = messages.slice(0, at).reverse().find((x) => x.role === "user")?.content ?? "";
            const title = (current.title && current.title !== FALLBACK_TITLE ? current.title : question.split("\n")[0]).slice(0, 120);
            setIssue({ title, body: m.content, repo: defaultRepo });
            break;
          }
        }
      } catch (e) {
        toast(errorMessage(e), "error");
      }
    },
    [api, current, messages, defaultRepo, open, refresh, send, toast, loopFrom],
  );
  // Rows are memoised; hand them one stable callback that always runs the latest handler.
  const actionRef = useRef(onAction);
  actionRef.current = onAction;
  const onActionStable = useCallback((m: ChatMessage, a: MessageAction) => void actionRef.current(m, a), []);

  const pin = (c: ChatMeta) => void api.patchChat(c.id, { pinned: !c.pinned }).then(upsertMeta, (e) => toast(errorMessage(e), "error"));
  const rename = (c: ChatMeta, title: string) => void api.patchChat(c.id, { title }).then(upsertMeta, (e) => toast(errorMessage(e), "error"));

  const remove = (c: ChatMeta) => {
    setHidden((h) => new Set(h).add(c.id));
    if (current?.id === c.id) reset();
    const timer = setTimeout(() => {
      deletes.current.delete(c.id);
      void api.deleteChat(c.id).then(
        () => refresh(),
        (e) => toast(errorMessage(e), "error"),
      );
    }, 6000);
    deletes.current.set(c.id, timer);
    toast({
      title: `Deleted “${(c.title || FALLBACK_TITLE).slice(0, 40)}”`,
      kind: "info",
      duration: 6000,
      action: {
        label: "Undo",
        onClick: () => {
          clearTimeout(deletes.current.get(c.id));
          deletes.current.delete(c.id);
          setHidden((h) => {
            const next = new Set(h);
            next.delete(c.id);
            return next;
          });
        },
      },
    });
  };

  const toggleSidebar = useCallback(
    () =>
      setCollapsed((c) => {
        store("colonizer.chat.sidebar", c ? null : "collapsed");
        return !c;
      }),
    [],
  );

  // Keyboard: ⌘\ sidebar, ⌘⇧O new conversation, ⌘F search this conversation, Esc stops a reply.
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key === "\\") {
        e.preventDefault();
        toggleSidebar();
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "o") {
        e.preventDefault();
        reset();
      } else if (mod && !e.shiftKey && e.key.toLowerCase() === "f" && current && messages.length > 0) {
        e.preventDefault();
        setSearch((s) => s ?? { query: "", index: 0 });
        setTimeout(() => searchInput.current?.select(), 0);
      } else if (e.key === "Escape" && busy && !document.querySelector("dialog[open]") && !document.querySelector(".chat-pop")) {
        abort.current?.abort();
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [busy, current, messages.length, reset, toggleSidebar]);

  const visible = useMemo(() => chats.filter((c) => !hidden.has(c.id)), [chats, hidden]);
  const hits = useMemo(() => (search ? searchMessages(messages, search.query) : []), [messages, search]);
  const hitIndex = search && hits.length ? ((search.index % hits.length) + hits.length) % hits.length : 0;
  useEffect(() => {
    if (!search || hits.length === 0) return;
    stick.current = false;
    document.getElementById(`msg-${hits[hitIndex]}`)?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [search, hits, hitIndex]);

  const candidates = useMemo(() => candidatesByParent(messages), [messages]);
  const lastReply = useMemo(() => [...messages].reverse().find((m) => m.role === "assistant" && !m.candidate)?.id, [messages]);
  const cost = useMemo(() => messages.reduce((n, m) => n + (m.cost_usd ?? 0), 0), [messages]);
  const tokens = useMemo(() => messages.reduce((n, m) => n + m.input_tokens + m.output_tokens, 0), [messages]);
  const suggestions = useMemo(() => suggestionsFor(org, repos, sessions), [org, repos, sessions]);
  // A path in a reply opens in the repository the conversation last attached a file or map from.
  const fileRepo = useMemo(() => {
    const note = messages.flatMap((m) => m.attachments ?? []).reverse().find((a) => a.kind === "file" || a.kind === "map");
    const repo = note?.label.split(/[ /]/).slice(0, 2).join("/");
    return repo && /^[\w.-]+\/[\w.-]+$/.test(repo) ? repo : defaultRepo;
  }, [messages, defaultRepo]);
  const openFile = useMemo(() => (onOpenFile && fileRepo ? (path: string) => onOpenFile(fileRepo, path) : undefined), [onOpenFile, fileRepo]);

  const personaName = current?.persona ?? settings.persona;
  const persona = useMemo(() => personaFor(personaName, personas), [personaName, personas]);
  // Replies are drawn as the conversation's ant; plain ones keep the provider's mark.
  const replyAnt = persona && persona.id !== "plain" ? persona : null;
  const system = current ? current.system ?? "" : settings.system;
  const temperature = current ? current.temperature ?? null : settings.temperature;
  const maxTokens = current ? current.max_tokens : settings.max_tokens;
  const empty = messages.length === 0 && live === null;

  const composer = (
    <ChatComposer
      draft={draft}
      onDraft={setDraft}
      attachments={attachments}
      onRemoveAttachment={(k) =>
        setAttachments((a) => {
          const gone = a.find((x) => x.key === k);
          if (gone?.preview?.startsWith("blob:")) URL.revokeObjectURL(gone.preview);
          return a.filter((x) => x.key !== k);
        })
      }
      model={model}
      models={models}
      claudeIds={claudeIds}
      onModel={setModel}
      modelOpen={modelOpen}
      compareModel={compareModel}
      onCompareModel={setCompareModel}
      busy={busy}
      blocked={draft.trim() ? blocked ?? (uploading ? "Waiting for the images to upload…" : null) : null}
      onSubmit={() => void submit()}
      onStop={() => abort.current?.abort()}
      sendKey={sendKey}
      onSendKey={(k) => {
        setSendKey(k);
        store("colonizer.chat.sendKey", k);
      }}
      attachMenu={<AttachMenu org={org} repos={repos} sessions={sessions} avatarFor={avatarFor} visionOk={vision} onAdd={addAttachment} onImage={addImage} control={attachControl} />}
      estimate={estimate}
      onImage={addImage}
      onSlash={onSlash}
      textarea={textarea}
      hero={empty}
    />
  );

  const row = (x: ChatMessage) => (
    <MessageRow
      key={x.id}
      m={x}
      models={models}
      claudeIds={claudeIds}
      isLastReply={x.id === lastReply}
      busy={busy}
      hit={search && hits.includes(x.id) ? (hits[hitIndex] === x.id ? "current" : "match") : null}
      onAction={onActionStable}
      onOpenFile={openFile}
      note={prefs.feedback[x.id] ?? null}
      imageUrl={api.chatImageUrl}
      onOpenImage={setLightbox}
      ant={replyAnt}
    />
  );

  return (
    <main className="cockpit flex min-h-0 flex-1">
      <ChatSidebar
        chats={visible}
        currentId={current?.id ?? null}
        models={models}
        workspaces={workspaces}
        collapsed={collapsed}
        onToggle={toggleSidebar}
        onNew={reset}
        onOpen={(id) => void open(id)}
        onPin={pin}
        onRename={rename}
        onDelete={remove}
      />

      <section aria-label="conversation" className="relative flex min-w-0 flex-1 flex-col">
        <header className="flex min-h-[52px] items-center gap-1.5 border-b border-border px-4 py-2">
          <div className="flex min-w-0 flex-1 items-center gap-2">
            {renamingTitle !== null && current ? (
              <input
                value={renamingTitle}
                autoFocus
                onFocus={(e) => e.target.select()}
                onChange={(e) => setRenamingTitle(e.target.value)}
                onBlur={() => {
                  if (renamingTitle.trim() && renamingTitle.trim() !== current.title) rename(current, renamingTitle.trim());
                  setRenamingTitle(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                  if (e.key === "Escape") setRenamingTitle(null);
                }}
                aria-label="conversation title"
                className="min-w-0 max-w-[420px] flex-1 rounded-md border border-accent bg-panel px-2 py-1 text-[14px] font-medium text-text outline-none"
              />
            ) : (
              <button
                type="button"
                disabled={!current}
                onClick={() => current && setRenamingTitle(current.title)}
                title={current ? "Rename" : undefined}
                className="min-w-0 cursor-pointer truncate rounded-md border-0 bg-transparent px-1 py-0.5 text-left text-[14px] font-medium text-text hover:bg-panel-2 disabled:cursor-default disabled:hover:bg-transparent"
              >
                {current?.title || FALLBACK_TITLE}
              </button>
            )}
            {current?.forked_from && (
              <button
                type="button"
                onClick={() => void open(current.forked_from!.chat)}
                title="Open the conversation this branched from"
                className="inline-flex shrink-0 cursor-pointer items-center gap-1 rounded-full border border-border bg-transparent px-2 py-0.5 text-[11px] text-faint hover:text-text"
              >
                <IconBranch size={11} /> branch
              </button>
            )}
          </div>

          <PersonaPicker personas={personas} value={persona} onPick={(p) => patch({ persona: p.name, system: p.system })} />
          <button
            ref={advancedButton}
            type="button"
            onClick={() => setAdvancedOpen((o) => !o)}
            aria-label="advanced settings"
            aria-expanded={advancedOpen}
            title="System prompt, temperature, max tokens (/system)"
            className={cx("grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent hover:bg-panel-2 hover:text-text", advancedOpen ? "text-text" : "text-faint")}
          >
            <IconSliders size={15} />
          </button>
          {current && messages.length > 0 && (
            <>
              <button
                type="button"
                onClick={() => {
                  setSearch((s) => (s ? null : { query: "", index: 0 }));
                  setTimeout(() => searchInput.current?.focus(), 0);
                }}
                aria-label="search this conversation"
                title={`Search this conversation (${MOD}F)`}
                className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-faint hover:bg-panel-2 hover:text-text"
              >
                <IconSearch size={15} />
              </button>
              <a
                href={api.chatExportUrl(current.id, hasImages(messages))}
                download={`${(current.title || "chat").replace(/[^\w.-]+/g, "-").slice(0, 60)}.${hasImages(messages) ? "zip" : "md"}`}
                aria-label="export as Markdown"
                title={hasImages(messages) ? "Export as Markdown, with the images beside it in a zip" : "Export as Markdown"}
                className="grid size-8 place-items-center rounded-lg text-faint hover:bg-panel-2 hover:text-text"
              >
                <IconDownload size={15} />
              </a>
              <span className="hidden pl-1 text-[11.5px] tabular-nums text-faint md:inline" title="This conversation's tokens and cost so far">
                {formatTokens(tokens)} tok{cost > 0 ? ` · ${formatCost(cost)}` : ""}
              </span>
            </>
          )}
          <Popover open={advancedOpen} onClose={() => setAdvancedOpen(false)} anchor={advancedButton} placement="bottom-end" width={420} label="advanced settings">
            <Advanced
              key={current?.id ?? "draft"}
              system={system}
              temperature={temperature}
              maxTokens={maxTokens}
              persona={persona}
              onSystem={(s) => patch({ system: s })}
              onTemperature={(t) => patch({ temperature: t ?? -1 })}
              onMaxTokens={(n) => patch({ max_tokens: n })}
              onSavePreset={(p, text) => {
                api.saveChatPersona(p.id, personaEdit(p.id, text)).then(
                  (next) => {
                    setPrefs(next);
                    toast(`Saved to the ${p.name} preset`, "success");
                  },
                  (e) => toast(errorMessage(e), "error"),
                );
              }}
            />
          </Popover>
        </header>

        {search && (
          <div className="flex items-center gap-2 border-b border-border bg-panel/60 px-4 py-1.5">
            <IconSearch size={14} className="text-faint" />
            <input
              ref={searchInput}
              value={search.query}
              onChange={(e) => setSearch({ query: e.target.value, index: 0 })}
              onKeyDown={(e) => {
                if (e.key === "Enter") setSearch((s) => s && { ...s, index: s.index + (e.shiftKey ? -1 : 1) });
                if (e.key === "Escape") setSearch(null);
              }}
              placeholder="Search this conversation"
              aria-label="search in conversation"
              className="min-w-0 flex-1 border-0 bg-transparent text-[13px] text-text outline-none placeholder:text-faint"
            />
            <span className="text-[11.5px] tabular-nums text-faint">{search.query ? (hits.length ? `${hitIndex + 1} of ${hits.length}` : "no matches") : ""}</span>
            <Button size="sm" variant="ghost" disabled={hits.length < 2} onClick={() => setSearch((s) => s && { ...s, index: s.index - 1 })} aria-label="previous match">
              ↑
            </Button>
            <Button size="sm" variant="ghost" disabled={hits.length < 2} onClick={() => setSearch((s) => s && { ...s, index: s.index + 1 })} aria-label="next match">
              ↓
            </Button>
            <button type="button" aria-label="close search" onClick={() => setSearch(null)} className="grid size-6 cursor-pointer place-items-center rounded border-0 bg-transparent text-faint hover:text-text">
              <IconX size={13} />
            </button>
          </div>
        )}

        {empty ? (
          <div className="scroll-thin flex min-h-0 flex-1 flex-col overflow-y-auto px-4">
            <div className="m-auto flex w-full max-w-[760px] flex-col items-center py-10">
              <LogoMark size={44} />
              <h2 className="m-0 mt-5 text-center text-[28px] font-semibold tracking-[-0.03em] text-text">What do you want to know?</h2>
              <p className="m-0 mt-2 max-w-[520px] text-center text-[13.5px] text-muted">
                Ask about your code, colonies and plans. Conversations stay on this mothership; only the model you pick sees them.
              </p>
              <div className="mt-7 w-full">{composer}</div>
              <div className="mt-6 grid w-full max-w-[720px] grid-cols-1 gap-2 sm:grid-cols-2 lg:grid-cols-3">
                {suggestions.slice(0, 6).map((s) => (
                  <button
                    key={s.id}
                    type="button"
                    onClick={() => {
                      setDraft(s.prompt);
                      if (s.attach) addAttachment(pending(s.attach, s.attach.kind === "map" ? 12000 : 4000));
                      if (s.step) attachControl.current?.(s.step);
                      else setTimeout(() => textarea.current?.focus(), 0);
                    }}
                    className="flex cursor-pointer flex-col gap-0.5 rounded-xl border border-border bg-panel/60 px-3.5 py-3 text-left transition-colors hover:border-border-strong hover:bg-panel"
                  >
                    <span className="text-[13px] font-medium leading-snug text-text">{s.title}</span>
                    <span className="truncate text-[11.5px] text-faint">{s.detail}</span>
                  </button>
                ))}
              </div>
            </div>
          </div>
        ) : (
          <>
            <div
              ref={scroller}
              onScroll={(e) => {
                const el = e.currentTarget;
                stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
              }}
              className="scroll-thin min-h-0 flex-1 overflow-y-auto px-4 py-6"
            >
              <div className="mx-auto flex max-w-[800px] flex-col gap-1">
                {messages.map((m) => {
                  if (m.candidate) return null;
                  const pair = m.role === "user" ? candidates.get(m.id) : undefined;
                  return (
                    <div key={m.id}>
                      {row(m)}
                      {pair && pair.length > 0 && (
                        <div className="my-2 grid gap-3 lg:grid-cols-2" aria-label="compare replies">
                          {pair.map((c) => (
                            <div key={c.id} className="min-w-0 rounded-xl border border-border bg-panel/50">
                              {row(c)}
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  );
                })}
                {live && (
                  <div className={cx(live.length > 1 && "my-2 grid gap-3 lg:grid-cols-2")}>
                    {live.map((l, i) =>
                      l.done ? null : (
                        <div key={i} className={cx(live.length > 1 && "min-w-0 rounded-xl border border-border bg-panel/50")}>
                          <StreamingRow model={l.model} text={l.text} models={models} ant={replyAnt} />
                        </div>
                      ),
                    )}
                  </div>
                )}
              </div>
            </div>
            <div className="px-4 pb-4 pt-2">{composer}</div>
          </>
        )}
      </section>

      {lightbox && <ImageLightbox {...lightbox} onClose={() => setLightbox(null)} />}
      {handoff && (
        <HandoffDialog
          repos={repos}
          org={org}
          avatarFor={avatarFor}
          initial={handoff}
          onClose={() => setHandoff(null)}
          onLaunch={async (repo, instructions) => {
            const session = await api.createSession({ repo, instructions, autopilot: autopilotDefault, origin: "chat" });
            toast(`Colony launched on ${repo}`);
            setHandoff(null);
            onCreated(session);
          }}
        />
      )}
      {loop && (
        <LoopDialog
          repos={repos}
          org={org}
          avatarFor={avatarFor}
          initial={loop}
          onClose={() => setLoop(null)}
          onCreate={async (l) => {
            await api.createLoop({ ...l, tz_offset_minutes: -new Date().getTimezoneOffset() });
            toast({ title: `Loop “${l.name}” created`, body: `On ${l.repo}. Manage it on the Loops page.`, kind: "success" });
            setLoop(null);
          }}
        />
      )}
      {issue && current && (
        <IssueDialog
          repos={repos}
          org={org}
          avatarFor={avatarFor}
          initial={issue}
          onClose={() => setIssue(null)}
          onFile={async (body) => {
            const r = await api.chatIssue(current.id, body);
            setIssue(null);
            toast({ title: "Issue created", body: r.url, kind: "success", action: { label: "Open", onClick: () => window.open(r.url, "_blank", "noopener") } });
          }}
        />
      )}
    </main>
  );
}

/** System prompt, temperature and max tokens; the system prompt can be saved back to its persona preset. */
function Advanced({
  system,
  temperature,
  maxTokens,
  persona,
  onSystem,
  onTemperature,
  onMaxTokens,
  onSavePreset,
}: {
  system: string;
  temperature: number | null;
  maxTokens: number;
  persona: Persona | null;
  onSystem: (s: string) => void;
  onTemperature: (t: number | null) => void;
  onMaxTokens: (n: number) => void;
  onSavePreset: (p: Persona, text: string) => void;
}): ReactElement {
  const [text, setText] = useState(system);
  const [temp, setTemp] = useState(temperature);
  const [max, setMax] = useState(String(maxTokens));
  const area = useRef<HTMLTextAreaElement>(null);
  useEffect(() => area.current?.focus(), []);
  return (
    <div className="flex flex-col gap-3 p-4">
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <span className="flex-1 text-[12.5px] font-medium text-muted">System prompt</span>
          {persona && persona.id !== "plain" && text !== persona.system && (
            <button type="button" onClick={() => onSavePreset(persona, text)} className="cursor-pointer border-0 bg-transparent p-0 text-[11.5px] text-accent hover:underline">
              Save to “{persona.name}”
            </button>
          )}
        </div>
        <textarea
          ref={area}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onBlur={() => text !== system && onSystem(text)}
          rows={7}
          placeholder="How the model should behave in this conversation…"
          aria-label="system prompt"
          className="scroll-thin resize-y rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-[13px] text-text outline-none placeholder:text-faint focus:border-accent"
        />
      </div>
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2 text-[12.5px] text-muted">
          <span className="flex-1 font-medium">Temperature</span>
          <label className="flex items-center gap-1.5 text-[12px]">
            <input
              type="checkbox"
              checked={temp === null}
              onChange={(e) => {
                const t = e.target.checked ? null : 0.7;
                setTemp(t);
                onTemperature(t);
              }}
            />
            provider default
          </label>
          <span className="w-9 text-right tabular-nums text-text">{temp === null ? "—" : temp.toFixed(2)}</span>
        </div>
        <input
          type="range"
          min={0}
          max={1}
          step={0.05}
          disabled={temp === null}
          value={temp ?? 0.7}
          onChange={(e) => setTemp(Number(e.target.value))}
          onPointerUp={() => temp !== null && onTemperature(temp)}
          onKeyUp={() => temp !== null && onTemperature(temp)}
          aria-label="temperature"
          className="accent-[var(--color-accent)] disabled:opacity-40"
        />
      </div>
      <label className="flex items-center gap-2 text-[12.5px] text-muted">
        <span className="flex-1 font-medium">Max tokens per reply</span>
        <input
          type="number"
          min={256}
          max={32000}
          step={256}
          value={max}
          onChange={(e) => setMax(e.target.value)}
          onBlur={() => {
            const n = Math.round(Number(max));
            if (Number.isFinite(n) && n >= 1 && n <= 32000 && n !== maxTokens) onMaxTokens(n);
            else setMax(String(maxTokens));
          }}
          aria-label="max tokens"
          className="w-24 rounded-lg border border-border bg-transparent px-2 py-1 text-right text-[13px] tabular-nums text-text outline-none focus:border-accent"
        />
      </label>
    </div>
  );
}
