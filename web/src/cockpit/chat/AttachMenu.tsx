// The composer's "+" menu: attach a repository file (a searchable tree), a colony, an architecture
// map or one of its components, a GitHub issue, a text snippet, an image, or a digest of today's
// colonies or this week's merged pull requests. Each becomes a chip; the server reads the content.
import { useEffect, useMemo, useRef, useState, type ReactElement, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { Avatar } from "../../components/Avatar";
import { IconFile, IconGitPR, IconImage, IconMap, IconPaperclip, IconPencil, IconPlus, IconSpark, IconAnt, IconChevron } from "../../components/icons";
import { Button, SESSION_STATUS, cx, orgOf, timeAgo } from "../../components/ui";
import type { ChatAttachment, Issue, Repo, Session } from "../../types";
import { Popover, SearchList, filterItems, type ListItem } from "./Popover";
import { attachmentLabel } from "./logic";

/** An attachment on the composer, with what is known of its size for the token preview. */
export interface Pending {
  key: string;
  attachment: ChatAttachment;
  label: string;
  /** Characters of context it adds, when known. */
  chars: number | null;
  /** An object or data: URL for an image chip's thumbnail. */
  preview?: string;
  /** An image still uploading to the mothership, 0–1; absent once it is stored. */
  progress?: number;
}

export function pending(attachment: ChatAttachment, chars: number | null, label?: string, preview?: string): Pending {
  return { key: `${attachment.kind}-${Math.random().toString(36).slice(2, 9)}`, attachment, label: label ?? attachmentLabel(attachment), chars, preview };
}

export type AttachStep = "menu" | "file-repo" | "file" | "colony" | "map-repo" | "map" | "issue-repo" | "issue" | "snippet";

const FOLDER = (
  <span aria-hidden="true" className="grid size-[18px] shrink-0 place-items-center text-faint">
    <IconChevron size={12} />
  </span>
);
const FILE = <IconFile size={15} className="shrink-0 text-faint" />;

function StepHeader({ title, onBack }: { title: ReactNode; onBack?: () => void }): ReactElement {
  return (
    <div className="flex items-center gap-2 border-b border-border px-2 py-1.5">
      {onBack && (
        <button type="button" onClick={onBack} aria-label="back" className="cursor-pointer rounded-md border-0 bg-transparent px-1.5 py-0.5 text-[13px] text-faint hover:bg-panel-2 hover:text-text">
          ←
        </button>
      )}
      <span className="min-w-0 flex-1 truncate text-[12.5px] font-medium text-muted">{title}</span>
    </div>
  );
}

/** Pick one of the workspace's repositories. */
function RepoStep({ repos, onPick, title, onBack, avatarFor }: { repos: readonly Repo[]; onPick: (repo: string) => void; title: string; onBack: () => void; avatarFor: (org: string) => string | null }): ReactElement {
  const items = useMemo<ListItem[]>(
    () =>
      [...repos]
        .sort((a, b) => (b.pushed_at ?? "").localeCompare(a.pushed_at ?? ""))
        .map((r) => {
          const owner = r.full_name.split("/")[0];
          return {
            id: r.full_name,
            label: r.full_name,
            hint: r.description ?? (r.pushed_at ? `pushed ${timeAgo(r.pushed_at)}` : undefined),
            leading: <Avatar name={owner} src={avatarFor(owner)} size={18} rounded="md" />,
          };
        }),
    [repos, avatarFor],
  );
  return <SearchList items={items} onPick={(i) => onPick(i.id)} placeholder="Search repositories…" emptyText="No repositories in this workspace." header={<StepHeader title={title} onBack={onBack} />} />;
}

/** A repository's files: browse folders, or type to search every path. */
function FileStep({ repo, onPick, onBack }: { repo: string; onPick: (path: string) => void; onBack: () => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [paths, setPaths] = useState<string[] | null>(null);
  const [dir, setDir] = useState("");
  const [query, setQuery] = useState("");
  useEffect(() => {
    let cancelled = false;
    api.repoTree(repo).then(
      (t) => !cancelled && setPaths(t.paths),
      (e) => {
        if (!cancelled) {
          setPaths([]);
          toast(errorMessage(e), "error");
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [api, repo, toast]);

  const items = useMemo<ListItem[]>(() => {
    if (!paths) return [];
    if (query.trim()) {
      const all = paths.map((p) => ({ id: p, label: p.split("/").pop() ?? p, hint: p, keywords: p, leading: FILE }));
      return filterItems(all, query).slice(0, 200);
    }
    const prefix = dir ? `${dir}/` : "";
    const dirs = new Set<string>();
    const files: string[] = [];
    for (const p of paths) {
      if (!p.startsWith(prefix)) continue;
      const rest = p.slice(prefix.length);
      const slash = rest.indexOf("/");
      if (slash >= 0) dirs.add(rest.slice(0, slash));
      else files.push(rest);
    }
    const up: ListItem[] = dir ? [{ id: "dir:..", label: "..", hint: "up one folder", leading: FOLDER }] : [];
    return [
      ...up,
      ...[...dirs].sort().map((d) => ({ id: `dir:${prefix}${d}`, label: `${d}/`, leading: FOLDER })),
      ...files.sort().map((f) => ({ id: `${prefix}${f}`, label: f, leading: FILE })),
    ];
  }, [paths, dir, query]);

  return (
    <SearchList
      items={items}
      loading={!paths}
      placeholder={`Search ${repo.split("/")[1]}…`}
      emptyText={query ? "No file matches." : "Empty folder."}
      onQueryChange={setQuery}
      header={<StepHeader title={`${repo}${dir ? ` / ${dir}` : ""}`} onBack={onBack} />}
      onPick={(i) => {
        if (i.id === "dir:..") setDir(dir.includes("/") ? dir.slice(0, dir.lastIndexOf("/")) : "");
        else if (i.id.startsWith("dir:")) setDir(i.id.slice(4));
        else onPick(i.id);
      }}
    />
  );
}

/** Colonies, newest activity first, with their org's logo, status and summary. */
function ColonyStep({ sessions, onPick, onBack, avatarFor }: { sessions: readonly Session[]; onPick: (s: Session) => void; onBack: () => void; avatarFor: (org: string) => string | null }): ReactElement {
  const items = useMemo(
    () =>
      [...sessions]
        .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
        .slice(0, 200)
        .map((s) => {
          const status = SESSION_STATUS[s.status];
          return {
            id: s.id,
            label: s.summary || s.issue_title || s.id,
            hint: `${s.repo}${s.issue ? ` #${s.issue}` : ""} · ${timeAgo(s.updated_at)}`,
            keywords: `${s.repo} ${s.status} ${s.issue_title}`,
            leading: <Avatar name={orgOf(s)} src={avatarFor(orgOf(s))} size={20} rounded="md" />,
            trailing: (
              <span className={cx("shrink-0 rounded-full px-1.5 py-px text-[10.5px]", status.live ? "bg-accent-soft text-accent" : status.tone === "err" ? "bg-err/10 text-err" : "bg-panel-2 text-muted")}>
                {status.label}
              </span>
            ),
            session: s,
          };
        }),
    [sessions, avatarFor],
  );
  return <SearchList items={items} onPick={(i) => onPick(i.session)} placeholder="Search colonies…" emptyText="No colonies yet." header={<StepHeader title="Attach a colony" onBack={onBack} />} />;
}

/** A repository's stored architecture map: the whole of it, or one component. */
function MapStep({ repo, onPick, onBack }: { repo: string; onPick: (a: ChatAttachment, label: string) => void; onBack: () => void }): ReactElement {
  const api = useApi();
  const [state, setState] = useState<{ items: ListItem[]; missing: boolean } | null>(null);
  useEffect(() => {
    let cancelled = false;
    api.repoMap(repo).then(
      (m) => {
        if (cancelled) return;
        const comps = m.map?.map.components ?? [];
        setState({
          missing: !m.map,
          items: m.map
            ? [
                { id: "*", label: "The whole map", hint: `${comps.length} components`, leading: <IconMap size={15} className="text-accent" /> },
                ...comps.map((c) => ({ id: c.id, label: c.label, hint: [c.type, c.sublabel].filter(Boolean).join(" · "), leading: <IconMap size={15} className="text-faint" /> })),
              ]
            : [],
        });
      },
      () => !cancelled && setState({ items: [], missing: true }),
    );
    return () => {
      cancelled = true;
    };
  }, [api, repo]);
  return (
    <SearchList
      items={state?.items ?? []}
      loading={!state}
      placeholder="Search components…"
      emptyText={state?.missing ? `${repo} has no map yet — map it from the Code or Nest page.` : "No component matches."}
      header={<StepHeader title={`${repo} map`} onBack={onBack} />}
      onPick={(i) => (i.id === "*" ? onPick({ kind: "map", repo }, `${repo.split("/")[1]} map`) : onPick({ kind: "map_component", repo, component: i.id }, i.label))}
    />
  );
}

/** A repository's open issues; the chosen one is attached as a snippet (title and body). */
function IssueStep({ repo, onPick, onBack }: { repo: string; onPick: (issue: Issue) => void; onBack: () => void }): ReactElement {
  const api = useApi();
  const [issues, setIssues] = useState<Issue[] | null>(null);
  useEffect(() => {
    let cancelled = false;
    api.issues(repo).then(
      (list) => !cancelled && setIssues(list),
      () => !cancelled && setIssues([]),
    );
    return () => {
      cancelled = true;
    };
  }, [api, repo]);
  const items = useMemo(
    () => (issues ?? []).map((i) => ({ id: String(i.number), label: `#${i.number} ${i.title}`, hint: i.labels.map((l) => l.name).join(", ") || `updated ${timeAgo(i.updatedAt)}`, issue: i })),
    [issues],
  );
  return <SearchList items={items} loading={!issues} placeholder="Search issues…" emptyText="No open issues." header={<StepHeader title={`${repo} issues`} onBack={onBack} />} onPick={(i) => onPick(i.issue)} />;
}

function SnippetStep({ onAdd, onBack }: { onAdd: (label: string, text: string) => void; onBack: () => void }): ReactElement {
  const [label, setLabel] = useState("");
  const [text, setText] = useState("");
  const area = useRef<HTMLTextAreaElement>(null);
  useEffect(() => area.current?.focus(), []);
  const over = text.length > 60 * 1024;
  return (
    <div className="flex flex-col">
      <StepHeader title="Paste a snippet" onBack={onBack} />
      <div className="flex flex-col gap-2 p-3">
        <input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Label (optional) — e.g. stack trace" aria-label="snippet label" className="rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-[13px] text-text outline-none placeholder:text-faint focus:border-accent" />
        <textarea
          ref={area}
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={8}
          placeholder="Logs, an error, a diff, notes…"
          aria-label="snippet text"
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && text.trim() && !over) onAdd(label.trim(), text);
          }}
          className="scroll-thin resize-y rounded-lg border border-border bg-transparent px-2.5 py-1.5 font-mono text-[12.5px] text-text outline-none placeholder:text-faint focus:border-accent"
        />
        <div className="flex items-center gap-2">
          <span className={cx("flex-1 text-[11.5px]", over ? "text-err" : "text-faint")}>{over ? "Over 60 KB" : `${text.length.toLocaleString()} characters`}</span>
          <Button variant="primary" size="sm" disabled={!text.trim() || over} onClick={() => onAdd(label.trim(), text)}>
            Attach
          </Button>
        </div>
      </div>
    </div>
  );
}

export function AttachMenu({
  org,
  repos,
  sessions,
  avatarFor,
  visionOk,
  onAdd,
  onImage,
  control,
}: {
  org: string | null;
  repos: readonly Repo[];
  sessions: readonly Session[];
  avatarFor: (org: string) => string | null;
  visionOk: boolean;
  /** Adds a chip, or updates the one with the same key (a file's size arrives after it is added). */
  onAdd: (p: Pending) => void;
  onImage: (file: File) => void;
  /** Lets slash commands and suggestion cards open the menu at a step. */
  control: { current: ((step: AttachStep) => void) | null };
}): ReactElement {
  const api = useApi();
  const [open, setOpen] = useState(false);
  const [step, setStep] = useState<AttachStep>("menu");
  const [repo, setRepo] = useState<string | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const scoped = useMemo(() => repos.filter((r) => !r.archived && (!org || r.full_name.toLowerCase().startsWith(`${org.toLowerCase()}/`))), [repos, org]);
  const orgSessions = useMemo(() => sessions.filter((s) => !org || orgOf(s).toLowerCase() === org.toLowerCase()), [sessions, org]);

  useEffect(() => {
    control.current = (s) => {
      setStep(s);
      setOpen(true);
    };
    return () => {
      control.current = null;
    };
  }, [control]);

  const close = () => {
    setOpen(false);
    setStep("menu");
  };
  const add = (p: Pending) => {
    onAdd(p);
    close();
  };

  const menu = useMemo(
    () => [
      { id: "file-repo", label: "Repository file", hint: "Browse or search a repo's tree", leading: <IconFile size={15} className="text-muted" /> },
      { id: "colony", label: "Colony", hint: "Summary, status and recent activity", leading: <IconAnt size={15} className="text-muted" /> },
      { id: "map-repo", label: "Architecture map", hint: "A stored map, or one component", leading: <IconMap size={15} className="text-muted" /> },
      { id: "issue-repo", label: "GitHub issue", hint: "Title and body of an open issue", leading: <IconGitPR size={15} className="text-muted" /> },
      { id: "snippet", label: "Text snippet", hint: "Logs, errors, notes (≤ 60 KB)", leading: <IconPencil size={15} className="text-muted" /> },
      {
        id: "image",
        label: "Image",
        hint: visionOk ? "Or paste / drop one on the composer" : "This model cannot see images",
        disabled: visionOk ? null : "This model cannot see images — pick a Claude or Anthropic-wire model",
        leading: <IconImage size={15} className="text-muted" />,
      },
      { id: "today", label: "Today's colonies", hint: org ? `What moved in ${org} in the last 24 h` : "What moved in the last 24 h", leading: <IconSpark size={15} className="text-muted" /> },
      { id: "merged", label: "Merged PRs · 7 days", hint: "For release notes and recaps", leading: <IconGitPR size={15} className="text-muted" /> },
    ],
    [visionOk, org],
  );

  let body: ReactElement;
  switch (step) {
    case "file-repo":
    case "map-repo":
    case "issue-repo":
      body = (
        <RepoStep
          repos={scoped}
          avatarFor={avatarFor}
          title={step === "file-repo" ? "File from which repository?" : step === "map-repo" ? "Map of which repository?" : "Issue from which repository?"}
          onBack={() => setStep("menu")}
          onPick={(r) => {
            setRepo(r);
            setStep(step === "file-repo" ? "file" : step === "map-repo" ? "map" : "issue");
          }}
        />
      );
      break;
    case "file":
      body = (
        <FileStep
          repo={repo ?? ""}
          onBack={() => setStep("file-repo")}
          onPick={(path) => {
            const r = repo ?? "";
            const p = pending({ kind: "file", repo: r, path }, null);
            add(p);
            // The size, for the token preview, once the blob answers.
            void api.repoBlob(r, path).then(
              (b) => onAdd({ ...p, chars: b.binary || b.too_large ? 0 : Math.min(b.text?.length ?? b.size, 60 * 1024) }),
              () => {},
            );
          }}
        />
      );
      break;
    case "colony":
      body = <ColonyStep sessions={orgSessions.length ? orgSessions : sessions} avatarFor={avatarFor} onBack={() => setStep("menu")} onPick={(s) => add(pending({ kind: "colony", id: s.id }, 3000, (s.summary || s.issue_title || s.id).slice(0, 48)))} />;
      break;
    case "map":
      body = <MapStep repo={repo ?? ""} onBack={() => setStep("map-repo")} onPick={(a, label) => add(pending(a, a.kind === "map" ? 12000 : 1500, label))} />;
      break;
    case "issue":
      body = (
        <IssueStep
          repo={repo ?? ""}
          onBack={() => setStep("issue-repo")}
          onPick={(i) => {
            const text = `${repo}#${i.number}: ${i.title}\n${i.url}\n\n${i.body ?? "(no description)"}`.slice(0, 60 * 1024);
            add(pending({ kind: "snippet", label: `#${i.number} ${i.title}`.slice(0, 60), text }, text.length));
          }}
        />
      );
      break;
    case "snippet":
      body = <SnippetStep onBack={() => setStep("menu")} onAdd={(label, text) => add(pending({ kind: "snippet", label: label || undefined, text }, text.length, label || "snippet"))} />;
      break;
    default:
      body = (
        <SearchList
          items={menu}
          searchable={false}
          header={<StepHeader title="Attach context" />}
          onPick={(i) => {
            if (i.id === "image") {
              fileInput.current?.click();
              close();
            } else if (i.id === "today") add(pending({ kind: "colonies_today", org: org ?? undefined }, 4000));
            else if (i.id === "merged") add(pending({ kind: "merged_prs", org: org ?? undefined, days: 7 }, 5000));
            else setStep(i.id as AttachStep);
          }}
        />
      );
  }

  return (
    <>
      <button
        ref={button}
        type="button"
        aria-label="attach context"
        title="Attach context"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => (open ? close() : setOpen(true))}
        className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-full border border-border bg-transparent text-muted hover:border-border-strong hover:text-text"
      >
        {open ? <IconPaperclip size={15} /> : <IconPlus size={16} />}
      </button>
      <input
        ref={fileInput}
        type="file"
        accept="image/png,image/jpeg,image/gif,image/webp"
        hidden
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) onImage(f);
          e.target.value = "";
        }}
      />
      <Popover open={open} onClose={close} anchor={button} placement="top-start" width={step === "snippet" ? 460 : 400} label="Attach context">
        <div className="flex max-h-[440px] min-h-0 flex-col">{body}</div>
      </Popover>
    </>
  );
}

