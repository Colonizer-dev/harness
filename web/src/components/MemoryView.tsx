import { useCallback, useEffect, useState } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { MemoryNote, MemoryProposal, MemoryScope, OrgInfo, Repo } from "../types";
import { IconCheck, IconMemory, IconMenu, IconOrg, IconPencil, IconPlus, IconTrash } from "./icons";
import { InlineCode, MarkdownBlock } from "./Markdown";
import { Badge, Button, Spinner, cx, inputClass, sameOrg, store, stored, timeAgo } from "./ui";

/** The org a proposal or note belongs to; null for global notes written by you. */
function noteOrg(note: MemoryNote): string | null {
  if (note.scope === "org") return note.key;
  if (note.scope === "repo") return note.key.split("/")[0];
  return "repo" in note.source ? note.source.repo.split("/")[0] : null;
}

export function MemoryView({
  narrow,
  selectedOrg,
  orgs,
  onOpenSidebar,
  onChanged,
}: {
  narrow: boolean;
  selectedOrg: string | null;
  orgs: OrgInfo[];
  onOpenSidebar: () => void;
  onChanged: () => void;
}) {
  const api = useApi();
  const [proposals, setProposals] = useState<MemoryProposal[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notesVersion, setNotesVersion] = useState(0);

  const loadProposals = useCallback(async () => {
    try {
      setProposals(await api.memoryProposals());
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void loadProposals();
    const timer = setInterval(loadProposals, 10_000);
    return () => clearInterval(timer);
  }, [loadProposals]);

  const visible = proposals?.filter((p) => !selectedOrg || noteOrg(p) === null || sameOrg(noteOrg(p), selectedOrg)) ?? [];
  const hidden = (proposals?.length ?? 0) - visible.length;

  const resolved = (id: string, approved: boolean) => {
    setProposals((list) => list?.filter((p) => p.id !== id) ?? null);
    if (approved) setNotesVersion((v) => v + 1);
    onChanged();
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="shrink-0 border-b border-border bg-panel px-4 py-3">
        <div className="flex items-start gap-3">
          {narrow && (
            <button
              type="button"
              onClick={onOpenSidebar}
              aria-label="Open sidebar"
              className="-ml-1.5 grid size-9 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
            >
              <IconMenu size={18} />
            </button>
          )}
          <div className="grid size-9 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
            <IconMemory size={18} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <h1 className="text-[17px] font-semibold leading-snug">Memory</h1>
              {selectedOrg && (
                <Badge>
                  <IconOrg size={11} /> {selectedOrg}
                </Badge>
              )}
            </div>
            <p className="mt-0.5 text-[12.5px] text-muted">
              Notes colonies can search while they work. Colonies propose new notes; nothing is shared until you approve it.
            </p>
          </div>
        </div>
      </header>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto w-full max-w-3xl space-y-8 px-4 py-6">
          <section aria-labelledby="proposals-title" className="space-y-3">
            <div className="flex flex-wrap items-center gap-2">
              <h2 id="proposals-title" className="text-[14.5px] font-semibold">
                Waiting for review
              </h2>
              {visible.length > 0 && <Badge tone="accent">{visible.length}</Badge>}
            </div>
            {error && <p className="text-[13px] text-err">{error}</p>}
            {!proposals && !error && (
              <p className="flex items-center gap-2 text-[13px] text-muted">
                <Spinner /> Loading proposals…
              </p>
            )}
            {proposals && visible.length === 0 && (
              <p className="rounded-xl border border-dashed border-border-strong px-4 py-5 text-center text-[13px] text-muted">
                Nothing to review. Colonies propose notes when they learn something worth keeping.
              </p>
            )}
            {visible.map((proposal) => (
              <ProposalCard key={proposal.id} proposal={proposal} onResolved={resolved} />
            ))}
            {hidden > 0 && (
              <p className="text-[12.5px] text-faint">
                {hidden} more for other orgs. Switch to All orgs to see {hidden === 1 ? "it" : "them"}.
              </p>
            )}
          </section>

          <NotesSection selectedOrg={selectedOrg} orgs={orgs} version={notesVersion} />
        </div>
      </div>
    </div>
  );
}

function ScopeBadge({ scope, keyName }: { scope: MemoryScope; keyName: string }) {
  if (scope === "global") return <Badge tone="accent">Global</Badge>;
  if (scope === "org")
    return (
      <Badge tone="info">
        <IconOrg size={11} /> {keyName}
      </Badge>
    );
  return (
    <Badge>
      <span className="font-mono font-medium">{keyName}</span>
    </Badge>
  );
}

function SourceLabel({ note }: { note: MemoryNote }) {
  if ("user" in note.source) return <span>Written by you</span>;
  return (
    <span className="min-w-0 [overflow-wrap:anywhere]">
      From colony <code className="font-mono text-[12px] text-text">{note.source.session_id}</code> on{" "}
      <span className="font-mono text-[12px]">{note.source.repo}</span>
    </span>
  );
}

function Tags({ tags }: { tags: string[] }) {
  if (!tags?.length) return null;
  return (
    <div className="mt-2 flex flex-wrap gap-1">
      {tags.map((tag) => (
        <span key={tag} className="rounded bg-panel-2 px-1.5 py-px font-mono text-[11.5px] text-muted">
          #{tag}
        </span>
      ))}
    </div>
  );
}

function ProposalCard({ proposal, onResolved }: { proposal: MemoryProposal; onResolved: (id: string, approved: boolean) => void }) {
  const api = useApi();
  const toast = useToast();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(proposal.title);
  const [content, setContent] = useState(proposal.content);
  const [busy, setBusy] = useState<"approve" | "reject" | null>(null);
  const edited = title !== proposal.title || content !== proposal.content;

  const run = async (kind: "approve" | "reject") => {
    setBusy(kind);
    try {
      if (kind === "approve") {
        await api.approveProposal(proposal.id, editing && edited ? { title: title.trim(), content } : undefined);
        toast(`Saved “${(editing ? title : proposal.title).trim()}” to memory`);
      } else {
        await api.rejectProposal(proposal.id);
        toast("Proposal rejected");
      }
      onResolved(proposal.id, kind === "approve");
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  return (
    <article className="rounded-xl border border-border bg-panel">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-4 pt-3 text-[12.5px] text-muted">
        <ScopeBadge scope={proposal.scope} keyName={proposal.key} />
        <SourceLabel note={proposal} />
        <span className="text-faint">· {timeAgo(proposal.created_at)}</span>
      </div>
      <div className="px-4 py-3">
        {editing ? (
          <div className="space-y-2">
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              aria-label="Note title"
              className={cx(inputClass, "font-semibold")}
            />
            <textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              aria-label="Note content (Markdown)"
              rows={Math.min(14, Math.max(4, content.split("\n").length + 1))}
              className={cx(inputClass, "resize-y font-mono text-[13px] leading-relaxed")}
            />
          </div>
        ) : (
          <>
            <h3 className="text-[14.5px] font-semibold [overflow-wrap:anywhere]">
              <InlineCode text={proposal.title} />
            </h3>
            <MarkdownBlock className="mt-1 text-text">{proposal.content}</MarkdownBlock>
            <Tags tags={proposal.tags} />
          </>
        )}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-4 py-2.5">
        <Button size="sm" variant="danger" className="mr-auto" disabled={busy !== null} onClick={() => run("reject")}>
          {busy === "reject" ? <Spinner /> : <IconTrash size={13} />} Reject
        </Button>
        {editing ? (
          <Button
            size="sm"
            variant="ghost"
            disabled={busy !== null}
            onClick={() => {
              setEditing(false);
              setTitle(proposal.title);
              setContent(proposal.content);
            }}
          >
            Cancel edit
          </Button>
        ) : (
          <Button size="sm" disabled={busy !== null} onClick={() => setEditing(true)}>
            <IconPencil size={13} /> Edit
          </Button>
        )}
        <Button
          size="sm"
          variant="primary"
          disabled={busy !== null || (editing && (!title.trim() || !content.trim()))}
          onClick={() => run("approve")}
        >
          {busy === "approve" ? <Spinner /> : <IconCheck size={13} />} {editing && edited ? "Approve edited" : "Approve"}
        </Button>
      </div>
    </article>
  );
}

/** Compact selects next to the scope tabs (inputClass is full width). */
const pickerClass =
  "h-8 min-w-0 max-w-full cursor-pointer rounded-lg border border-border bg-panel pl-2.5 pr-1.5 text-text outline-none focus:border-accent focus:ring-2 focus:ring-[var(--accent-ring)]";

const SCOPES: { id: MemoryScope; label: string }[] = [
  { id: "global", label: "Global" },
  { id: "org", label: "Organisation" },
  { id: "repo", label: "Repository" },
];

function NotesSection({ selectedOrg, orgs, version }: { selectedOrg: string | null; orgs: OrgInfo[]; version: number }) {
  const api = useApi();
  const toast = useToast();
  const [scope, setScope] = useState<MemoryScope>(() => {
    const saved = stored("colonizer.memory.scope");
    return saved === "org" || saved === "repo" ? saved : "global";
  });
  const [repos, setRepos] = useState<Repo[]>([]);
  const [org, setOrg] = useState<string>(selectedOrg ?? "");
  const [repo, setRepo] = useState<string>("");
  const [notes, setNotes] = useState<MemoryNote[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    api.repos().then(setRepos).catch(() => {});
  }, [api]);

  useEffect(() => {
    store("colonizer.memory.scope", scope);
  }, [scope]);

  const orgNames = [...new Set([...orgs.map((o) => o.org), ...repos.map((r) => r.full_name.split("/")[0])])].sort((a, b) =>
    a.localeCompare(b),
  );

  // Follow the sidebar's workspace; otherwise default to the first org we know.
  useEffect(() => {
    if (selectedOrg) setOrg(selectedOrg);
    else setOrg((current) => current || orgNames[0] || "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedOrg, orgNames.join(",")]);

  const orgRepos = repos.filter((r) => sameOrg(r.full_name.split("/")[0], org));
  useEffect(() => {
    if (!orgRepos.some((r) => r.full_name === repo)) setRepo(orgRepos[0]?.full_name ?? "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [org, repos]);

  const key = scope === "global" ? "" : scope === "org" ? org : repo;
  const ready = scope === "global" || key !== "";

  const load = useCallback(async () => {
    if (!ready) {
      setNotes([]);
      return;
    }
    try {
      const listing = await api.memory(scope, key);
      setNotes(listing.notes);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api, scope, key, ready]);

  useEffect(() => {
    setNotes(null);
    setAdding(false);
    void load();
  }, [load, version]);

  const remove = async (note: MemoryNote) => {
    if (!window.confirm(`Delete “${note.title}”? Colonies will no longer see it.`)) return;
    try {
      await api.deleteNote(note);
      setNotes((list) => list?.filter((n) => n.id !== note.id) ?? null);
      toast("Note deleted");
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const description =
    scope === "global"
      ? "Every colony in every org sees these."
      : scope === "org"
        ? org
          ? `Colonies on any ${org} repository see these.`
          : "Pick an organisation."
        : repo
          ? `Only colonies on ${repo} see these.`
          : "Pick a repository.";

  return (
    <section aria-labelledby="notes-title" className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id="notes-title" className="mr-auto text-[14.5px] font-semibold">
          Notes
        </h2>
        {!adding && (
          <Button size="sm" variant="primary" disabled={!ready} onClick={() => setAdding(true)}>
            <IconPlus size={13} /> Add note
          </Button>
        )}
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <div role="tablist" aria-label="Memory scope" className="inline-flex rounded-lg bg-panel-2 p-0.5">
          {SCOPES.map((s) => (
            <button
              key={s.id}
              type="button"
              role="tab"
              aria-selected={scope === s.id}
              onClick={() => setScope(s.id)}
              className={cx(
                "cursor-pointer rounded-md px-2.5 py-1 text-[12.5px] font-medium transition-colors",
                scope === s.id ? "bg-panel text-text shadow-sm" : "text-muted hover:text-text",
              )}
            >
              {s.label}
            </button>
          ))}
        </div>
        {scope !== "global" && (
          <select
            value={org}
            onChange={(e) => setOrg(e.target.value)}
            aria-label="Organisation"
            className={cx(pickerClass, "text-[13px]")}
          >
            {orgNames.length === 0 && <option value="">No organisations</option>}
            {orgNames.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
        )}
        {scope === "repo" && (
          <select
            value={repo}
            onChange={(e) => setRepo(e.target.value)}
            aria-label="Repository"
            className={cx(pickerClass, "font-mono text-[12.5px]")}
          >
            {orgRepos.length === 0 && <option value="">No repositories</option>}
            {orgRepos.map((r) => (
              <option key={r.full_name} value={r.full_name}>
                {r.full_name.split("/")[1]}
              </option>
            ))}
          </select>
        )}
      </div>
      <p className="text-[12.5px] text-muted">{description}</p>

      {adding && ready && (
        <NoteForm
          scope={scope}
          keyName={key}
          onCancel={() => setAdding(false)}
          onCreated={(note) => {
            setAdding(false);
            setNotes((list) => [note, ...(list ?? [])]);
          }}
        />
      )}

      {error && <p className="text-[13px] text-err">{error}</p>}
      {!notes && !error && (
        <p className="flex items-center gap-2 text-[13px] text-muted">
          <Spinner /> Loading notes…
        </p>
      )}
      {notes && notes.length === 0 && !adding && (
        <p className="rounded-xl border border-dashed border-border-strong px-4 py-5 text-center text-[13px] text-muted">
          No notes here yet.
        </p>
      )}
      {notes?.map((note) => (
        <article key={note.id} className="group rounded-xl border border-border bg-panel px-4 py-3">
          <div className="flex items-start gap-2">
            <div className="min-w-0 flex-1">
              <h3 className="text-[14px] font-semibold [overflow-wrap:anywhere]">
                <InlineCode text={note.title} />
              </h3>
              <div className="mt-0.5 flex flex-wrap gap-x-1.5 text-[12px] text-faint">
                <SourceLabel note={note} />
                <span>· {timeAgo(note.created_at)}</span>
              </div>
            </div>
            <button
              type="button"
              onClick={() => remove(note)}
              aria-label={`Delete note ${note.title}`}
              title="Delete note"
              className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-faint hover:bg-err-soft hover:text-err"
            >
              <IconTrash size={14} />
            </button>
          </div>
          <MarkdownBlock className="mt-1.5 text-text">{note.content}</MarkdownBlock>
          <Tags tags={note.tags} />
        </article>
      ))}
    </section>
  );
}

function NoteForm({
  scope,
  keyName,
  onCancel,
  onCreated,
}: {
  scope: MemoryScope;
  keyName: string;
  onCancel: () => void;
  onCreated: (note: MemoryNote) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const [saving, setSaving] = useState(false);

  return (
    <form
      className="space-y-2 rounded-xl border border-accent/40 bg-panel p-3.5"
      onSubmit={async (e) => {
        e.preventDefault();
        setSaving(true);
        try {
          const note = await api.createNote({ scope, key: keyName, title: title.trim(), content: content.trim() });
          toast("Note added");
          onCreated(note);
        } catch (error) {
          toast(errorMessage(error), "error");
          setSaving(false);
        }
      }}
    >
      <div className="flex flex-wrap items-center gap-2 text-[12.5px] text-muted">
        New note in <ScopeBadge scope={scope} keyName={keyName} />
      </div>
      <input
        autoFocus
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        placeholder="Title, e.g. Run tests with pnpm test:unit"
        aria-label="Title"
        className={inputClass}
      />
      <textarea
        value={content}
        onChange={(e) => setContent(e.target.value)}
        placeholder="What colonies should know. Markdown works."
        aria-label="Content (Markdown)"
        rows={5}
        className={cx(inputClass, "resize-y font-mono text-[13px] leading-relaxed")}
      />
      <div className="flex justify-end gap-2">
        <Button size="sm" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button size="sm" type="submit" variant="primary" disabled={saving || !title.trim() || !content.trim()}>
          {saving && <Spinner />} Save note
        </Button>
      </div>
    </form>
  );
}
