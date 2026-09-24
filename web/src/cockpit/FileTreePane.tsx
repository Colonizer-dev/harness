// The map's explorer: the repository's files as a VS Code-style tree in a right-hand pane, with the
// chosen component's files marked and their folders opened, and the files live colonies are changing
// flagged. Paths come from GET /api/maps/{owner}/{repo}/files (the mothership's local clone).
import { useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { cx } from "../components/ui";
import { FileDetail } from "./FileDetail";

export interface TreeNode {
  name: string;
  path: string;
  dir: boolean;
  children: TreeNode[];
}

/** Paths → a sorted tree: folders first, then files, each alphabetical (as VS Code sorts). */
export function buildTree(paths: readonly string[]): TreeNode {
  const root: TreeNode = { name: "", path: "", dir: true, children: [] };
  const dirs = new Map<string, TreeNode>([["", root]]);
  for (const path of paths) {
    const parts = path.split("/");
    let parent = root;
    for (let i = 0; i < parts.length; i++) {
      const sub = parts.slice(0, i + 1).join("/");
      const isDir = i < parts.length - 1;
      if (isDir) {
        let node = dirs.get(sub);
        if (!node) {
          node = { name: parts[i], path: sub, dir: true, children: [] };
          dirs.set(sub, node);
          parent.children.push(node);
        }
        parent = node;
      } else {
        parent.children.push({ name: parts[i], path: sub, dir: false, children: [] });
      }
    }
  }
  const sort = (node: TreeNode) => {
    node.children.sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name) : a.dir ? -1 : 1));
    node.children.forEach(sort);
  };
  sort(root);
  return root;
}

/** Every ancestor folder of the given paths ("a/b/c.rs" → "a", "a/b"). A marked folder counts too. */
export function ancestorsOf(paths: Iterable<string>): Set<string> {
  const out = new Set<string>();
  for (const p of paths) {
    const parts = p.split("/");
    for (let i = 1; i < parts.length; i++) out.add(parts.slice(0, i).join("/"));
    out.add(p);
  }
  return out;
}

/** Whether `path` is marked: listed itself, or inside a marked folder. */
function isMarked(path: string, marked: ReadonlySet<string>): boolean {
  if (marked.has(path)) return true;
  for (const m of marked) if (path.startsWith(`${m}/`)) return true;
  return false;
}

export function FileTreePane({
  repo,
  revision,
  paths,
  error,
  title,
  marked,
  changing,
  reading = new Set<string>(),
  componentOf = () => null,
  onOpenColony = () => {},
  initialDetail = null,
  onClose,
}: {
  repo: string;
  revision: string | null;
  /** Null while loading. */
  paths: string[] | null;
  error: string | null;
  /** The chosen component's name, as the pane's heading. */
  title: string;
  /** The component's source paths (files or folders). */
  marked: ReadonlySet<string>;
  /** Files live colonies are changing. */
  changing: ReadonlySet<string>;
  /** Files live colonies have been reading lately. */
  reading?: ReadonlySet<string>;
  /** The map component a file belongs to, for the file detail's header. */
  componentOf?: (path: string) => string | null;
  onOpenColony?: (id: string) => void;
  /** A file to open on; tests pin the detail view through it. */
  initialDetail?: string | null;
  onClose: () => void;
}): ReactElement {
  // A clicked file opens its detail in place of the tree; the back arrow or Escape returns.
  const [detail, setDetail] = useState<string | null>(initialDetail);
  const tree = useMemo(() => (paths ? buildTree(paths) : null), [paths]);
  const [open, setOpen] = useState<Set<string>>(() => ancestorsOf(marked));
  const [onlyMarked, setOnlyMarked] = useState(false);
  const [query, setQuery] = useState("");
  const firstMarked = useRef<HTMLDivElement | null>(null);
  // Choosing another component opens its folders (keeping what the user opened) and scrolls to it.
  useEffect(() => {
    setOpen((cur) => new Set([...cur, ...ancestorsOf(marked)]));
  }, [marked]);
  useEffect(() => {
    firstMarked.current?.scrollIntoView({ block: "center" });
  }, [marked, tree]);

  const q = query.trim().toLowerCase();
  const visibleMarks = ancestorsOf(marked);
  const markedCount = paths ? paths.filter((p) => isMarked(p, marked)).length : 0;
  let scrolled = false;

  const shown = (node: TreeNode): boolean => {
    if (onlyMarked && !visibleMarks.has(node.path) && !isMarked(node.path, marked)) return false;
    if (!q) return true;
    if (node.path.toLowerCase().includes(q)) return true;
    return node.dir && node.children.some(shown);
  };

  const row = (node: TreeNode, depth: number): ReactElement | null => {
    if (!shown(node)) return null;
    const mark = isMarked(node.path, marked);
    const change = !node.dir && changing.has(node.path);
    const expanded = node.dir && (open.has(node.path) || Boolean(q));
    const holds = node.dir && !mark && visibleMarks.has(node.path);
    const ref = mark && !scrolled ? ((scrolled = true), firstMarked) : undefined;
    return (
      <div key={node.path} role="treeitem" aria-expanded={node.dir ? expanded : undefined} aria-selected={mark}>
        <div
          ref={ref}
          onClick={() =>
            node.dir
              ? setOpen((cur) => {
                  const next = new Set(cur);
                  if (next.has(node.path)) next.delete(node.path);
                  else next.add(node.path);
                  return next;
                })
              : setDetail(node.path)
          }
          title={node.path}
          className={cx(
            "flex h-[22px] items-center gap-1 whitespace-nowrap pr-2 font-mono text-[12px]",
            "cursor-pointer",
            mark ? "bg-accent-soft text-accent" : holds ? "text-text" : "text-muted",
            !mark && "hover:bg-panel-2",
          )}
          style={{ paddingLeft: 8 + depth * 12 }}
        >
          <span aria-hidden="true" className="inline-block w-3 shrink-0 text-center text-[9px] text-faint">
            {node.dir ? (expanded ? "▾" : "▸") : ""}
          </span>
          <span aria-hidden="true" className="shrink-0 text-[11px]">
            {node.dir ? (expanded ? "📂" : "📁") : "📄"}
          </span>
          <span className="truncate">{node.name}</span>
          {holds && <span aria-hidden="true" className="ml-1 size-1.5 shrink-0 rounded-full bg-accent" />}
          {change ? (
            <span className="ml-auto shrink-0 rounded px-1 text-[10px] font-semibold text-warn" title="a live colony is changing this file — click to see the diff">
              M
            </span>
          ) : (
            !node.dir &&
            reading.has(node.path) && (
              <span aria-label="a live colony is reading this file" title="a live colony is reading this file — click to see what it does" className="ml-auto size-1.5 shrink-0 rounded-full border border-accent" />
            )
          )}
        </div>
        {expanded && <div role="group">{node.children.map((c) => row(c, depth + 1))}</div>}
      </div>
    );
  };

  return (
    <aside
      aria-label={`files · ${title}`}
      onKeyDown={(e) => {
        if (e.key === "Escape" && detail) {
          e.stopPropagation();
          setDetail(null);
        }
      }}
      className="flex h-full w-[380px] shrink-0 flex-col border-l border-border bg-panel"
    >
      {detail ? (
        <FileDetail repo={repo} path={detail} component={componentOf(detail)} onBack={() => setDetail(null)} onOpenColony={onOpenColony} />
      ) : (
      <>
      <div className="flex items-start gap-2 border-b border-border px-3 py-2.5">
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-semibold text-text">{title}</div>
          <div className="truncate font-mono text-[11px] text-faint">
            {repo}
            {revision ? ` @ ${revision.slice(0, 7)}` : ""} · {markedCount} {markedCount === 1 ? "file" : "files"} marked
          </div>
        </div>
        <button type="button" aria-label="close files" onClick={onClose} className="grid size-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ×
        </button>
      </div>
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter files…"
          aria-label="filter files"
          className="min-w-0 flex-1 rounded-md border border-border bg-transparent px-2 py-1 text-[12px] text-text outline-none placeholder:text-faint focus:border-border-strong"
        />
        <label className="flex shrink-0 cursor-pointer items-center gap-1 text-[11.5px] text-muted">
          <input type="checkbox" checked={onlyMarked} onChange={(e) => setOnlyMarked(e.target.checked)} />
          marked only
        </label>
      </div>
      <div role="tree" aria-label="repository files" className="scroll-thin min-h-0 flex-1 overflow-auto py-1">
        {error ? (
          <p className="px-3 py-2 text-[12.5px] text-err">{error}</p>
        ) : !tree ? (
          <p className="px-3 py-2 text-[12.5px] text-faint">Loading files…</p>
        ) : (
          tree.children.map((c) => row(c, 0))
        )}
      </div>
      </>
      )}
    </aside>
  );
}
