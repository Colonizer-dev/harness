// The conversation list: search, a workspace filter with org logos, pinned conversations first
// then Today / Yesterday / Previous 7 days / Older. Rows rename inline, pin, and delete (with an
// undo toast, handled by the view). Collapses to a thin rail with ⌘\.
import { useMemo, useRef, useState, type ReactElement } from "react";
import { Avatar } from "../../components/Avatar";
import { ProviderMark } from "../../components/providerMark";
import { IconPencil, IconPin, IconPlus, IconSearch, IconSidebar, IconTrash, IconBranch } from "../../components/icons";
import { cx, timeAgo } from "../../components/ui";
import type { ChatMeta, ChatModels } from "../../types";
import { Select, type ListItem } from "./Popover";
import { chatMatches, groupChats, personaLabel } from "./logic";
import { markFor } from "./ModelChip";

function Row({
  c,
  active,
  models,
  onOpen,
  onPin,
  onRename,
  onDelete,
}: {
  c: ChatMeta;
  active: boolean;
  models: ChatModels | null;
  onOpen: () => void;
  onPin: () => void;
  onRename: (title: string) => void;
  onDelete: () => void;
}): ReactElement {
  const [renaming, setRenaming] = useState<string | null>(null);
  const mark = markFor(c.model, models);
  const title = c.title || "New conversation";
  if (renaming !== null) {
    const commit = () => {
      const t = renaming.trim();
      if (t && t !== c.title) onRename(t);
      setRenaming(null);
    };
    return (
      <div className="px-1 py-0.5">
        <input
          value={renaming}
          autoFocus
          onFocus={(e) => e.target.select()}
          onChange={(e) => setRenaming(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") setRenaming(null);
          }}
          aria-label="conversation title"
          className="w-full rounded-lg border border-accent bg-panel px-2 py-1.5 text-[13px] text-text outline-none"
        />
      </div>
    );
  }
  return (
    <div className={cx("group/row relative flex items-center rounded-lg", active ? "bg-panel-2" : "hover:bg-panel-2/60")}>
      <button
        type="button"
        onClick={onOpen}
        onDoubleClick={() => setRenaming(c.title)}
        onKeyDown={(e) => {
          if (e.key === "F2") setRenaming(c.title);
          if (e.key === "Delete" || (e.key === "Backspace" && (e.metaKey || e.ctrlKey))) onDelete();
        }}
        aria-current={active ? "true" : undefined}
        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 border-0 bg-transparent px-2 py-1.5 text-left"
      >
        <ProviderMark preset={mark.preset} name={mark.name} size="button" />
        <span className="min-w-0 flex-1">
          <span className={cx("flex items-center gap-1 truncate text-[13px]", active ? "text-text" : "text-muted group-hover/row:text-text")}>
            {c.forked_from && <IconBranch size={11} className="shrink-0 text-faint" />}
            <span className="truncate">{title}</span>
          </span>
          <span className="block truncate text-[11px] text-faint">
            {c.persona && c.persona !== "Plain" ? `${personaLabel(c.persona)} · ` : ""}
            {timeAgo(c.updated_at)}
          </span>
        </span>
      </button>
      <div className={cx("absolute right-1 flex items-center gap-0.5 rounded-md bg-panel-2 pl-1", "opacity-0 focus-within:opacity-100 group-hover/row:opacity-100")}>
        <button type="button" aria-label={c.pinned ? `unpin ${title}` : `pin ${title}`} title={c.pinned ? "Unpin" : "Pin"} onClick={onPin} className={cx("grid size-6 cursor-pointer place-items-center rounded border-0 bg-transparent hover:text-text", c.pinned ? "text-accent" : "text-faint")}>
          <IconPin size={13} />
        </button>
        <button type="button" aria-label={`rename ${title}`} title="Rename (F2)" onClick={() => setRenaming(c.title)} className="grid size-6 cursor-pointer place-items-center rounded border-0 bg-transparent text-faint hover:text-text">
          <IconPencil size={13} />
        </button>
        <button type="button" aria-label={`delete ${title}`} title="Delete" onClick={onDelete} className="grid size-6 cursor-pointer place-items-center rounded border-0 bg-transparent text-faint hover:text-err">
          <IconTrash size={13} />
        </button>
      </div>
      {c.pinned && <IconPin size={11} className="pointer-events-none absolute right-2 top-2 text-accent group-hover/row:hidden" />}
    </div>
  );
}

export function ChatSidebar({
  chats,
  currentId,
  models,
  workspaces,
  collapsed,
  onToggle,
  onNew,
  onOpen,
  onPin,
  onRename,
  onDelete,
}: {
  chats: readonly ChatMeta[];
  currentId: string | null;
  models: ChatModels | null;
  workspaces: readonly { org: string; avatar: string | null }[];
  collapsed: boolean;
  onToggle: () => void;
  onNew: () => void;
  onOpen: (id: string) => void;
  onPin: (c: ChatMeta) => void;
  onRename: (c: ChatMeta, title: string) => void;
  onDelete: (c: ChatMeta) => void;
}): ReactElement {
  const [query, setQuery] = useState("");
  const [workspace, setWorkspace] = useState<string | null>(null);
  const search = useRef<HTMLInputElement>(null);
  const groups = useMemo(() => groupChats(chats.filter((c) => chatMatches(c, query, workspace))), [chats, query, workspace]);
  const orgItems = useMemo<ListItem[]>(
    () => [
      { id: "", label: "All workspaces", leading: <span className="grid size-[18px] place-items-center rounded-md bg-panel-2 text-[10px] text-faint">∗</span> },
      ...workspaces.map((w) => ({ id: w.org, label: w.org, leading: <Avatar name={w.org} src={w.avatar} size={18} rounded="md" /> })),
    ],
    [workspaces],
  );

  if (collapsed) {
    return (
      <aside aria-label="conversations" className="flex w-[52px] shrink-0 flex-col items-center gap-2 border-r border-border py-3">
        <button type="button" onClick={onToggle} aria-label="show conversations (⌘\)" title="Show conversations (⌘\)" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          <IconSidebar size={16} />
        </button>
        <button type="button" onClick={onNew} aria-label="new conversation" title="New conversation (⌘⇧O)" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          <IconPlus size={16} />
        </button>
      </aside>
    );
  }

  return (
    <aside aria-label="conversations" className="flex w-[272px] shrink-0 flex-col border-r border-border bg-panel/40">
      <div className="flex items-center gap-1 px-3 pb-2 pt-3">
        <button type="button" onClick={onToggle} aria-label="hide conversations (⌘\)" title="Hide conversations (⌘\)" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          <IconSidebar size={16} />
        </button>
        <h1 className="m-0 flex-1 text-[15px] font-semibold tracking-tight">Chats</h1>
        <button
          type="button"
          onClick={onNew}
          aria-label="new conversation"
          title="New conversation (⌘⇧O)"
          className="inline-flex cursor-pointer items-center gap-1 rounded-lg border border-border bg-panel px-2 py-1 text-[12.5px] text-text hover:border-border-strong"
        >
          <IconPlus size={14} /> New
        </button>
      </div>
      <div className="flex flex-col gap-2 px-3 pb-2">
        <label className="flex items-center gap-2 rounded-lg border border-border bg-panel px-2.5 py-1.5 focus-within:border-accent">
          <IconSearch size={14} className="text-faint" />
          <input
            ref={search}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setQuery("")}
            placeholder="Search conversations"
            aria-label="search conversations"
            className="bare-field min-w-0 flex-1 border-0 bg-transparent text-[13px] text-text outline-none placeholder:text-faint"
          />
        </label>
        {workspaces.length > 1 && (
          <Select
            value={workspace ?? ""}
            items={orgItems}
            onChange={(i) => setWorkspace(i.id || null)}
            ariaLabel="workspace filter"
            width={260}
            className="w-full"
          />
        )}
      </div>
      <nav className="scroll-thin min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        {groups.length === 0 && <p className="px-2 py-3 text-[12.5px] text-faint">{chats.length === 0 ? "No conversations yet. Ask something to start one." : "Nothing matches."}</p>}
        {groups.map((g) => (
          <section key={g.label} aria-label={g.label} className="mb-2">
            <h2 className="m-0 px-2 pb-1 pt-2 text-[10.5px] font-semibold uppercase tracking-wider text-faint">{g.label}</h2>
            {g.chats.map((c) => (
              <Row
                key={c.id}
                c={c}
                active={c.id === currentId}
                models={models}
                onOpen={() => onOpen(c.id)}
                onPin={() => onPin(c)}
                onRename={(t) => onRename(c, t)}
                onDelete={() => onDelete(c)}
              />
            ))}
          </section>
        ))}
      </nav>
    </aside>
  );
}
