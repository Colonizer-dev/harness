// The Code page's editor: a VS Code-like view of one repository at one branch, read from the
// mothership's bare clone. Explorer, tabs and Monaco (lazy chunk); a branch switcher; per-file
// history with compare, and blame; "Ask AI" about the file; edits autosaved as drafts on the
// mothership (if enabled) and sent to GitHub only by Create PR, after an explicit confirm.
import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import ReactMarkdown from "react-markdown";
import { errorMessage, useApi, useToast } from "../context";
import type { Draft, FileCommit, RepoBlame, RepoBranch, Session } from "../types";
import { Button, Spinner, cx, timeAgo, useMediaQuery } from "../components/ui";
import { buildTree, type TreeNode } from "./FileTreePane";
import { monacoLanguage, suggestBranch, unifiedDiff } from "./code";

const MonacoPane = lazy(() => import("./MonacoPane"));

interface Tab {
  path: string;
  /** The file at the branch head when opened (what the edit is against). */
  original: string;
  content: string;
  /** The branch head commit `original` was read at. */
  baseSha: string;
  binary?: boolean;
  tooLarge?: boolean;
  /** Restored from a draft saved on the mothership. */
  restored?: boolean;
  /** The file changed upstream since the draft's base: the upstream text, until resolved. */
  conflict?: string | null;
}

const FALLBACK_KEY = "colonizer.codeDrafts";

/** The localStorage copy: an offline fallback only, never the source of truth. */
function saveFallback(repo: string, ref: string, path: string, content: string | null): void {
  try {
    const all = JSON.parse(localStorage.getItem(FALLBACK_KEY) ?? "{}") as Record<string, string>;
    const key = `${repo}\u0000${ref}\u0000${path}`;
    if (content === null) delete all[key];
    else all[key] = content;
    localStorage.setItem(FALLBACK_KEY, JSON.stringify(all));
  } catch {
    // Storage blocked or full: the mothership copy is what counts.
  }
}

function Explorer({ tree, active, dirty, onOpen }: { tree: TreeNode | null; active: string | null; dirty: Set<string>; onOpen: (path: string) => void }): ReactElement {
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");
  const q = filter.trim().toLowerCase();
  const shown = (n: TreeNode): boolean => !q || n.path.toLowerCase().includes(q) || (n.dir && n.children.some(shown));
  const row = (n: TreeNode, depth: number): ReactElement | null => {
    if (!shown(n)) return null;
    const expanded = n.dir && (open.has(n.path) || Boolean(q));
    return (
      <div key={n.path} role="treeitem" aria-expanded={n.dir ? expanded : undefined} aria-selected={active === n.path}>
        <button
          type="button"
          onClick={() =>
            n.dir
              ? setOpen((cur) => {
                  const next = new Set(cur);
                  if (next.has(n.path)) next.delete(n.path);
                  else next.add(n.path);
                  return next;
                })
              : onOpen(n.path)
          }
          title={n.path}
          className={cx(
            "flex h-[22px] w-full cursor-pointer items-center gap-1 border-0 bg-transparent pr-2 text-left font-mono text-[12px]",
            active === n.path ? "bg-accent-soft text-accent" : "text-muted hover:bg-panel-2 hover:text-text",
          )}
          style={{ paddingLeft: 6 + depth * 12 }}
        >
          <span aria-hidden="true" className="w-3 shrink-0 text-center text-[9px] text-faint">
            {n.dir ? (expanded ? "▾" : "▸") : ""}
          </span>
          <span className="truncate">{n.name}</span>
          {dirty.has(n.path) && <span aria-label="modified" className="ml-auto size-1.5 shrink-0 rounded-full bg-accent" />}
        </button>
        {expanded && <div role="group">{n.children.map((c) => row(c, depth + 1))}</div>}
      </div>
    );
  };
  return (
    <aside aria-label="explorer" className="flex w-[260px] shrink-0 flex-col border-r border-border bg-panel">
      <div className="px-2 py-2">
        <input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter files…" aria-label="filter files" className="w-full rounded-md border border-border bg-transparent px-2 py-1 text-[12px] text-text outline-none placeholder:text-faint focus:border-border-strong" />
      </div>
      <div role="tree" aria-label="repository files" className="scroll-thin min-h-0 flex-1 overflow-auto pb-2">
        {tree ? tree.children.map((c) => row(c, 0)) : <p className="px-3 text-[12px] text-faint">Loading files…</p>}
      </div>
    </aside>
  );
}

function BranchSwitcher({ branches, value, onPick }: { branches: RepoBranch[]; value: string; onPick: (name: string) => void }): ReactElement {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const list = branches.filter((b) => b.name.toLowerCase().includes(q.trim().toLowerCase()));
  return (
    <div className="relative">
      <button type="button" aria-haspopup="listbox" aria-expanded={open} onClick={() => setOpen((o) => !o)} className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-panel px-2.5 font-mono text-[12px] text-text hover:border-border-strong">
        <span aria-hidden="true">⎇</span>
        <span className="max-w-[220px] truncate">{value}</span>
        <span aria-hidden="true" className="text-faint">▾</span>
      </button>
      {open && (
        <>
          <div aria-hidden="true" className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
          <div role="listbox" aria-label="branches" onKeyDown={(e) => e.key === "Escape" && setOpen(false)} className="v3-pop absolute left-0 top-9 z-50 flex max-h-[420px] w-[440px] flex-col overflow-hidden rounded-xl border border-border-strong shadow-[0_16px_48px_rgb(0_0_0/0.4)]">
            <input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder="Find a branch…" aria-label="find a branch" className="m-2 rounded-md border border-border bg-transparent px-2 py-1 text-[12.5px] text-text outline-none" />
            <div className="scroll-thin min-h-0 flex-1 overflow-y-auto pb-1">
              {list.map((b) => (
                <button
                  key={b.name}
                  type="button"
                  role="option"
                  aria-selected={b.name === value}
                  onClick={() => {
                    setOpen(false);
                    onPick(b.name);
                  }}
                  className={cx("flex w-full cursor-pointer flex-col gap-0.5 border-0 px-3 py-1.5 text-left hover:bg-panel-2", b.name === value ? "bg-panel-2" : "bg-transparent")}
                >
                  <span className="flex items-center gap-1.5">
                    <span className="truncate font-mono text-[12.5px] text-text">{b.name}</span>
                    {b.default && <span className="rounded bg-accent-soft px-1 text-[10px] text-accent">default</span>}
                    {b.protected && <span className="rounded bg-panel-3 px-1 text-[10px] text-muted">protected</span>}
                    {b.colony && <span className="rounded bg-panel-3 px-1 text-[10px] text-muted">colony</span>}
                    {!b.default && (
                      <span className="ml-auto shrink-0 font-mono text-[10.5px] text-faint" title="ahead / behind the default branch">
                        ↑{b.ahead} ↓{b.behind}
                      </span>
                    )}
                  </span>
                  <span className="flex items-center gap-1.5 text-[11px] text-faint">
                    <span className="truncate">{b.message}</span>
                    <span className="shrink-0">· {b.author} · {timeAgo(b.date)}</span>
                    {b.pr && (
                      <a href={b.pr.url} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()} className="ml-auto shrink-0 text-accent">
                        #{b.pr.number}
                        {b.pr.isDraft ? " draft" : ""}
                      </a>
                    )}
                  </span>
                </button>
              ))}
            </div>
          </div>
        </>
      )}
    </div>
  );
}

function CreatePrDialog({
  repo,
  branches,
  current,
  tabs,
  onClose,
  onDone,
}: {
  repo: string;
  branches: RepoBranch[];
  current: string;
  tabs: Tab[];
  onClose: () => void;
  onDone: (url: string, base: string) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const ref = useRef<HTMLDialogElement>(null);
  const defaultBranch = branches.find((b) => b.default)?.name ?? current;
  const [base, setBase] = useState(current);
  const [branch, setBranch] = useState(() => suggestBranch(tabs.map((t) => t.path)));
  const [message, setMessage] = useState(`Edit ${tabs.map((t) => t.path.split("/").pop()).join(", ")}`.slice(0, 120));
  const [title, setTitle] = useState(message);
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  const diff = useMemo(() => tabs.map((t) => unifiedDiff(t.path, t.original, t.content)).join("\n\n"), [tabs]);
  useEffect(() => {
    if (ref.current && !ref.current.open) ref.current.showModal();
  }, []);
  const submit = async () => {
    setBusy(true);
    try {
      const r = await api.createEdits(repo, { base, branch, message, title, body, files: tabs.map((t) => ({ path: t.path, content: t.content })) });
      onDone(r.url, r.base);
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  const field = "w-full rounded-md border border-border bg-transparent px-2 py-1.5 text-[13px] text-text outline-none focus:border-border-strong";
  return (
    <dialog ref={ref} onClose={onClose} aria-labelledby="create-pr-title" className="m-auto w-[min(820px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50">
      <div className="flex max-h-[calc(100dvh-24px)] flex-col">
        <h2 id="create-pr-title" className="m-0 border-b border-border px-5 py-3 text-[16px] font-semibold">
          Create a pull request · <span className="font-mono text-[13px] text-muted">{repo}</span>
        </h2>
        <div className="scroll-thin grid min-h-0 flex-1 gap-3 overflow-y-auto px-5 py-4 sm:grid-cols-2">
          <label className="text-[12px] text-muted">
            Into (base)
            <select value={base} onChange={(e) => setBase(e.target.value)} className={field}>
              {[current, defaultBranch, ...branches.map((b) => b.name)]
                .filter((v, i, a) => a.indexOf(v) === i)
                .map((b) => (
                  <option key={b} value={b}>
                    {b}
                  </option>
                ))}
            </select>
          </label>
          <label className="text-[12px] text-muted">
            New branch
            <input value={branch} onChange={(e) => setBranch(e.target.value)} className={cx(field, "font-mono")} />
          </label>
          <label className="text-[12px] text-muted sm:col-span-2">
            Commit message
            <input value={message} onChange={(e) => setMessage(e.target.value)} className={field} />
          </label>
          <label className="text-[12px] text-muted sm:col-span-2">
            Pull request title
            <input value={title} onChange={(e) => setTitle(e.target.value)} className={field} />
          </label>
          <label className="text-[12px] text-muted sm:col-span-2">
            Description
            <textarea value={body} onChange={(e) => setBody(e.target.value)} rows={3} className={field} />
          </label>
          <div className="sm:col-span-2">
            <div className="mb-1 text-[12px] text-muted">
              {tabs.length} {tabs.length === 1 ? "file" : "files"} · the whole change
            </div>
            <pre className="scroll-thin m-0 max-h-[300px] overflow-auto rounded-lg border border-border bg-panel-2 p-3 font-mono text-[11.5px] leading-relaxed">
              {diff.split("\n").map((l, i) => (
                <div key={i} className={l.startsWith("+") && !l.startsWith("+++") ? "bg-ok/10 text-ok" : l.startsWith("-") && !l.startsWith("---") ? "bg-err/10 text-err" : "text-muted"}>
                  {l || " "}
                </div>
              ))}
            </pre>
          </div>
        </div>
        <div className="flex items-center gap-2 border-t border-border px-5 py-3">
          <span className="mr-auto text-[12px] text-muted">Pushes branch {branch} and opens the pull request on GitHub as you.</span>
          <Button onClick={() => ref.current?.close()}>Cancel</Button>
          <Button variant="primary" disabled={busy || !branch.trim() || !title.trim() || !message.trim()} onClick={() => void submit()}>
            {busy && <Spinner />} Create pull request
          </Button>
        </div>
      </div>
    </dialog>
  );
}

export default function CodeEditor({
  repo,
  initialPath,
  onClose,
  onCreated,
  onOpenColony,
}: {
  repo: string;
  /** A file to open once the tree has loaded (the Chat view's "open in Code"). */
  initialPath?: string | null;
  onClose: () => void;
  onCreated: (s: Session) => void;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const narrow = useMediaQuery("(max-width: 899px)");
  const [branches, setBranches] = useState<RepoBranch[]>([]);
  const [ref, setRef] = useState<string | null>(null);
  const [treeSha, setTreeSha] = useState<string | null>(null);
  const [tree, setTree] = useState<TreeNode | null>(null);
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [panel, setPanel] = useState<"history" | "ask" | null>(null);
  const [blameOn, setBlameOn] = useState(false);
  const [blame, setBlame] = useState<RepoBlame | null>(null);
  const [history, setHistory] = useState<FileCommit[] | null>(null);
  const [compare, setCompare] = useState<{ label: string; text: string } | null>(null);
  const [showDiff, setShowDiff] = useState(false);
  const [full, setFull] = useState(false);
  const [autosave, setAutosave] = useState(true);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState<{ text: string; model: string } | null>(null);
  const [asking, setAsking] = useState(false);
  const [selection, setSelection] = useState<[number, number] | null>(null);
  const [, tick] = useState(0);
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const tab = tabs.find((t) => t.path === active) ?? null;
  const dirtyTabs = tabs.filter((t) => t.content !== t.original);
  const dirty = new Set(dirtyTabs.map((t) => t.path));

  // Branches and the editor setting, once.
  useEffect(() => {
    api.editorSettings().then((s) => setAutosave(s.autosave), () => {});
    api.repoBranches(repo).then(
      (b) => {
        setBranches(b.branches);
        setRef((r) => r ?? b.default);
      },
      (e) => {
        toast(errorMessage(e), "error");
        setRef((r) => r ?? "main");
      },
    );
  }, [api, repo, toast]);

  // The tree and any drafts for the current ref.
  useEffect(() => {
    if (!ref) return;
    let cancelled = false;
    setTree(null);
    setTabs([]);
    setActive(null);
    (async () => {
      try {
        const t = await api.repoTree(repo, ref);
        if (cancelled) return;
        setTree(buildTree(t.paths));
        setTreeSha(t.sha);
        const d = await api.drafts(repo, ref).catch(() => ({ drafts: [] as Draft[] }));
        const restored: Tab[] = [];
        for (const draft of d.drafts) {
          const head = await api.repoBlob(repo, draft.path, t.sha).catch(() => null);
          const headText = head?.text ?? "";
          let conflict: string | null = null;
          if (draft.base_sha !== t.sha) {
            const base = await api.repoBlob(repo, draft.path, draft.base_sha).catch(() => null);
            if ((base?.text ?? "") !== headText) conflict = headText;
          }
          restored.push({ path: draft.path, original: headText, content: draft.content, baseSha: t.sha, restored: true, conflict });
        }
        if (cancelled) return;
        if (restored.length > 0) {
          setTabs(restored);
          setActive(restored[0].path);
          toast({ title: `Restored ${restored.length} ${restored.length === 1 ? "draft" : "drafts"}`, body: "Edits autosaved on this mothership, not yet on GitHub.", kind: "info" });
        }
      } catch (e) {
        if (!cancelled) toast(errorMessage(e), "error");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api, repo, ref, toast]);

  const open = useCallback(
    async (path: string) => {
      setActive(path);
      setCompare(null);
      if (tabs.some((t) => t.path === path) || !ref) return;
      try {
        const b = await api.repoBlob(repo, path, ref);
        const text = b.text ?? "";
        setTabs((ts) => (ts.some((t) => t.path === path) ? ts : [...ts, { path, original: text, content: text, baseSha: b.sha, binary: b.binary, tooLarge: b.too_large }]));
      } catch (e) {
        toast(errorMessage(e), "error");
      }
    },
    [api, repo, ref, tabs, toast],
  );

  // The file another view asked for, once the tree it lives in has loaded.
  const openedInitial = useRef<string | null>(null);
  useEffect(() => {
    if (!initialPath || !tree || openedInitial.current === initialPath) return;
    openedInitial.current = initialPath;
    void open(initialPath);
  }, [initialPath, tree, open]);

  const persist = useCallback(
    (t: Tab) => {
      if (!ref) return;
      const clean = t.content === t.original;
      saveFallback(repo, ref, t.path, clean ? null : t.content);
      if (!autosave) return;
      const run = clean ? api.deleteDrafts(repo, ref, t.path).then(() => null) : api.saveDraft(repo, { ref, path: t.path, content: t.content, base_sha: t.baseSha }).then((r) => r.saved_at);
      run.then(
        (at) => {
          if (at) setSavedAt(Date.parse(at));
          setSaveError(null);
        },
        (e) => setSaveError(errorMessage(e)),
      );
    },
    [api, autosave, ref, repo],
  );

  const edit = (value: string) => {
    if (!tab) return;
    const next = { ...tab, content: value };
    setTabs((ts) => ts.map((t) => (t.path === tab.path ? next : t)));
    const timer = timers.current.get(tab.path);
    if (timer) clearTimeout(timer);
    timers.current.set(tab.path, setTimeout(() => persist(next), 1000));
  };

  const close = (path: string) => {
    const t = tabs.find((x) => x.path === path);
    if (t && t.content !== t.original && !autosave) {
      toast({ title: "Unsaved changes", body: "Autosave is off: create a pull request or turn autosave on before closing this file.", kind: "warn" });
      return;
    }
    setTabs((ts) => ts.filter((x) => x.path !== path));
    if (active === path) setActive(tabs.find((x) => x.path !== path)?.path ?? null);
  };

  const discard = (path: string) => {
    setTabs((ts) => ts.map((t) => (t.path === path ? { ...t, content: t.original, restored: false, conflict: null } : t)));
    if (ref) {
      void api.deleteDrafts(repo, ref, path).catch(() => {});
      saveFallback(repo, ref, path, null);
    }
  };

  // Blame and history follow the open file and ref.
  useEffect(() => {
    setBlame(null);
    if (!blameOn || !tab || !ref) return;
    api.fileBlame(repo, tab.path, ref).then(setBlame, (e) => toast(errorMessage(e), "error"));
  }, [api, blameOn, ref, repo, tab?.path, toast]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    setHistory(null);
    if (panel !== "history" || !tab || !ref) return;
    api.fileHistory(repo, tab.path, ref).then((h) => setHistory(h.commits), (e) => toast(errorMessage(e), "error"));
  }, [api, panel, ref, repo, tab?.path, toast]); // eslint-disable-line react-hooks/exhaustive-deps

  // Keys: ⌘⇧F full screen, Esc leaves it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setFull((f) => !f);
      } else if (e.key === "Escape" && full) {
        setFull(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [full]);

  // Without autosave, leaving the page with unsaved edits asks first.
  useEffect(() => {
    if (autosave || dirtyTabs.length === 0) return;
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [autosave, dirtyTabs.length]);

  useEffect(() => {
    const t = setInterval(() => tick((n) => n + 1), 5000);
    return () => clearInterval(t);
  }, []);

  const toggleAutosave = async () => {
    const next = !autosave;
    try {
      const r = await api.saveEditorSettings({ autosave: next });
      setAutosave(r.autosave);
      if (r.autosave) for (const t of dirtyTabs) persist(t);
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const ask = async () => {
    if (!tab || !question.trim()) return;
    setAsking(true);
    setAnswer(null);
    try {
      const r = await api.askFile(repo, { path: tab.path, question, content: tab.content, selection });
      setAnswer({ text: r.answer, model: r.model });
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setAsking(false);
    }
  };

  const handToColony = async () => {
    if (!tab || !question.trim()) return;
    const lines = selection ? ` (lines ${selection[0]}–${selection[1]})` : "";
    try {
      const s = await api.createSession({
        repo,
        instructions: `On branch ${ref ?? "the default branch"}, about \`${tab.path}\`${lines}:\n\n${question.trim()}\n\nOpen a pull request with the change if one is needed.`,
        autopilot: true,
      });
      onCreated(s);
      toast({ title: "Handed to a colony", body: `${tab.path}${lines}`, kind: "success", action: { label: "Watch it work", onClick: () => onOpenColony(s.id) } });
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const readOnly = narrow || !!tab?.binary || !!tab?.tooLarge;
  const diffOriginal = compare?.text ?? (tab?.conflict != null ? tab.conflict : showDiff && tab && tab.content !== tab.original ? tab.original : null);
  const iconButton = (on: boolean) =>
    cx("inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg border px-2.5 text-[12.5px]", on ? "border-accent bg-accent-soft text-accent" : "border-border bg-panel text-muted hover:border-border-strong hover:text-text");

  return (
    <section aria-label={`editor · ${repo}`} className={cx("flex min-h-0 flex-1 flex-col bg-bg", full && "fixed inset-0 z-[90]")}>
      <header className="flex flex-wrap items-center gap-2 border-b border-border px-3 py-2">
        <button type="button" onClick={onClose} className="cursor-pointer rounded-md border-0 bg-transparent px-2 py-1 text-[12.5px] text-muted hover:bg-panel-2 hover:text-text" aria-label="back to the Code page">
          ←
        </button>
        <span className="font-mono text-[13px] font-semibold text-text">{repo}</span>
        {ref && <BranchSwitcher branches={branches} value={ref} onPick={(b) => (dirtyTabs.length > 0 && !autosave ? toast({ title: "Unsaved changes", body: "Turn autosave on or create a pull request before switching branches.", kind: "warn" }) : setRef(b))} />}
        <span className="flex-1" />
        <button type="button" className={iconButton(blameOn)} aria-pressed={blameOn} onClick={() => setBlameOn((b) => !b)} disabled={!tab}>
          Blame
        </button>
        <button type="button" className={iconButton(panel === "history")} aria-pressed={panel === "history"} onClick={() => setPanel((p) => (p === "history" ? null : "history"))} disabled={!tab}>
          History
        </button>
        <button type="button" className={iconButton(showDiff)} aria-pressed={showDiff} onClick={() => setShowDiff((d) => !d)} disabled={!tab || tab.content === tab.original}>
          Diff
        </button>
        <button type="button" className={iconButton(panel === "ask")} aria-pressed={panel === "ask"} onClick={() => setPanel((p) => (p === "ask" ? null : "ask"))} disabled={!tab}>
          Ask AI
        </button>
        <Button variant="primary" disabled={dirtyTabs.length === 0 || narrow} onClick={() => setCreating(true)}>
          Create PR{dirtyTabs.length > 0 ? ` · ${dirtyTabs.length}` : ""}
        </Button>
        <button type="button" className={iconButton(full)} aria-pressed={full} title="Full screen (⌘⇧F, Esc to leave)" onClick={() => setFull((f) => !f)}>
          {full ? "Exit full screen" : "Full screen"}
        </button>
      </header>

      <div className="flex min-h-0 flex-1">
        {!narrow && <Explorer tree={tree} active={active} dirty={dirty} onOpen={(p) => void open(p)} />}
        <div className="flex min-w-0 flex-1 flex-col">
          <div role="tablist" aria-label="open files" className="scroll-thin flex shrink-0 overflow-x-auto border-b border-border bg-panel">
            {tabs.map((t) => (
              <div key={t.path} role="tab" aria-selected={t.path === active} className={cx("flex shrink-0 items-center gap-1.5 border-r border-border px-3 py-1.5 font-mono text-[12px]", t.path === active ? "bg-bg text-text" : "text-muted")}>
                <button type="button" onClick={() => setActive(t.path)} className="cursor-pointer border-0 bg-transparent p-0 text-inherit" title={t.path}>
                  {t.path.split("/").pop()}
                </button>
                {t.content !== t.original && <span aria-label="modified" className="size-1.5 rounded-full bg-accent" />}
                <button type="button" aria-label={`close ${t.path}`} onClick={() => close(t.path)} className="cursor-pointer border-0 bg-transparent p-0 text-faint hover:text-text">
                  ×
                </button>
              </div>
            ))}
          </div>
          {tab?.restored && tab.conflict == null && tab.content !== tab.original && (
            <div className="flex items-center gap-2 border-b border-border bg-accent-soft px-3 py-1.5 text-[12px] text-accent">
              Restored draft — autosaved on this mothership, not yet on GitHub.
              <button type="button" onClick={() => discard(tab.path)} className="ml-auto cursor-pointer border-0 bg-transparent text-[12px] underline">
                Discard draft
              </button>
            </div>
          )}
          {tab?.conflict != null && (
            <div className="flex flex-wrap items-center gap-2 border-b border-border bg-warn/10 px-3 py-1.5 text-[12px] text-warn">
              This file changed on {ref} since the draft was made. Left: upstream now; right: your draft.
              <button type="button" onClick={() => setTabs((ts) => ts.map((t) => (t.path === tab.path ? { ...t, conflict: null } : t)))} className="ml-auto cursor-pointer rounded border border-warn/40 bg-transparent px-2 py-0.5 text-[12px] text-warn">
                Keep mine
              </button>
              <button type="button" onClick={() => discard(tab.path)} className="cursor-pointer rounded border border-warn/40 bg-transparent px-2 py-0.5 text-[12px] text-warn">
                Take upstream
              </button>
            </div>
          )}
          {compare && (
            <div className="flex items-center gap-2 border-b border-border bg-panel-2 px-3 py-1.5 text-[12px] text-muted">
              Comparing {compare.label} (left) with the current file (right).
              <button type="button" onClick={() => setCompare(null)} className="ml-auto cursor-pointer border-0 bg-transparent text-[12px] underline">
                Close compare
              </button>
            </div>
          )}
          <div className="min-h-0 flex-1">
            {!tab ? (
              <div className="flex h-full items-center justify-center text-[13px] text-faint">{tree ? "Open a file from the explorer." : "Loading…"}</div>
            ) : tab.binary ? (
              <div className="flex h-full items-center justify-center text-[13px] text-faint">Binary file — not shown.</div>
            ) : tab.tooLarge ? (
              <div className="flex h-full items-center justify-center text-[13px] text-faint">Larger than 1 MB — not shown.</div>
            ) : (
              <Suspense
                fallback={
                  <div className="flex h-full items-center justify-center gap-2 text-[13px] text-muted">
                    <Spinner /> Loading the editor…
                  </div>
                }
              >
                <MonacoPane
                  uri={`colonizer://${repo}/${ref}/${tab.path}`}
                  value={tab.content}
                  language={monacoLanguage(tab.path)}
                  readOnly={readOnly}
                  onChange={edit}
                  onSelection={setSelection}
                  diffOriginal={diffOriginal}
                  blame={blameOn && tab.content === tab.original ? blame : null}
                />
              </Suspense>
            )}
          </div>
          <footer className="flex shrink-0 items-center gap-3 border-t border-border bg-panel px-3 py-1 text-[11.5px] text-faint">
            {tab && <span className="font-mono">{tab.path}</span>}
            {treeSha && <span className="font-mono">@ {treeSha.slice(0, 7)}</span>}
            {narrow && <span>read-only on a narrow screen</span>}
            <span className="flex-1" />
            {saveError ? (
              <span className="text-err" title={saveError}>
                Not saved locally
              </span>
            ) : autosave ? (
              <span>{savedAt ? `Saved locally · ${timeAgo(new Date(savedAt).toISOString())}` : "Autosave on"}</span>
            ) : (
              <span>Autosave off</span>
            )}
            <label className="flex cursor-pointer items-center gap-1">
              <input type="checkbox" checked={autosave} onChange={() => void toggleAutosave()} />
              Autosave drafts on this mothership
            </label>
          </footer>
        </div>

        {panel === "history" && tab && (
          <aside aria-label="file history" className="scroll-thin w-[300px] shrink-0 overflow-y-auto border-l border-border bg-panel p-3">
            <h3 className="m-0 mb-2 text-[13px] font-semibold text-text">History · {tab.path.split("/").pop()}</h3>
            {!history ? (
              <p className="text-[12px] text-faint">Loading…</p>
            ) : history.length === 0 ? (
              <p className="text-[12px] text-faint">No commits touch this file on {ref}.</p>
            ) : (
              <ul className="m-0 list-none space-y-1 p-0">
                {history.map((c) => (
                  <li key={c.sha}>
                    <button
                      type="button"
                      onClick={async () => {
                        try {
                          const b = await api.repoBlob(repo, tab.path, c.sha);
                          setCompare({ label: `${c.sha.slice(0, 7)} · ${c.message}`, text: b.text ?? "" });
                        } catch (e) {
                          toast(errorMessage(e), "error");
                        }
                      }}
                      className="w-full cursor-pointer rounded-md border-0 bg-transparent px-2 py-1.5 text-left hover:bg-panel-2"
                    >
                      <span className="block truncate text-[12.5px] text-text">{c.message}</span>
                      <span className="block font-mono text-[11px] text-faint">
                        {c.sha.slice(0, 7)} · {c.author} · {timeAgo(c.date)}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </aside>
        )}

        {panel === "ask" && tab && (
          <aside aria-label="ask AI about this file" className="flex w-[340px] shrink-0 flex-col gap-2 border-l border-border bg-panel p-3">
            <h3 className="m-0 text-[13px] font-semibold text-text">Ask about {tab.path.split("/").pop()}</h3>
            <p className="m-0 text-[11.5px] text-faint">{selection ? `About lines ${selection[0]}–${selection[1]}.` : "Select lines to ask about just those."}</p>
            <textarea value={question} onChange={(e) => setQuestion(e.target.value)} rows={4} placeholder="What does this do? Where is X handled?" aria-label="your question" className="w-full rounded-md border border-border bg-transparent px-2 py-1.5 text-[13px] text-text outline-none focus:border-border-strong" />
            <div className="flex gap-2">
              <Button variant="primary" disabled={!question.trim() || asking} onClick={() => void ask()}>
                {asking && <Spinner />} Answer here
              </Button>
              <Button disabled={!question.trim()} onClick={() => void handToColony()}>
                Hand to a colony
              </Button>
            </div>
            <div className="scroll-thin min-h-0 flex-1 overflow-y-auto text-[13px] leading-relaxed text-text">
              {answer && (
                <>
                  <div className="prose-sm">
                    <ReactMarkdown>{answer.text}</ReactMarkdown>
                  </div>
                  <p className="mt-2 text-[11px] text-faint">Answered by {answer.model}</p>
                </>
              )}
            </div>
          </aside>
        )}
      </div>

      {creating && ref && (
        <CreatePrDialog
          repo={repo}
          branches={branches}
          current={ref}
          tabs={dirtyTabs}
          onClose={() => setCreating(false)}
          onDone={(url, base) => {
            setCreating(false);
            for (const t of dirtyTabs) {
              void api.deleteDrafts(repo, ref, t.path).catch(() => {});
              saveFallback(repo, ref, t.path, null);
            }
            setTabs((ts) => ts.map((t) => ({ ...t, original: t.content, restored: false, conflict: null })));
            toast({ title: "Pull request opened", body: `Into ${base}`, kind: "success", action: { label: "Open on GitHub", onClick: () => window.open(url, "_blank", "noopener") } });
          }}
        />
      )}
    </section>
  );
}
