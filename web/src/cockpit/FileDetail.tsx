// One file in the map's explorer: which live colonies are on it, what their ants did there (the
// tool calls that named the file) and each one's diff of it, rendered like a unified diff on
// GitHub. Live: re-read every few seconds while open (GET /api/maps/{owner}/{repo}/file).
import { useEffect, useState, type ReactElement } from "react";
import { AntAvatar } from "../components/AntAvatar";
import { SESSION_STATUS, cx, timeAgo } from "../components/ui";
import { errorMessage, useApi } from "../context";
import type { MapFileColony, MapFileDetail } from "../types";

/** How often an open file detail is re-read. */
const POLL_MS = 5000;

export type DiffLine =
  | { kind: "hunk"; text: string }
  | { kind: "meta"; text: string }
  | { kind: "add" | "del" | "ctx"; text: string; old: number | null; new: number | null };

/** A unified diff → rows with old/new line numbers taken from each hunk header. */
export function parseDiff(diff: string): DiffLine[] {
  const out: DiffLine[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  for (const line of diff.replace(/\n$/, "").split("\n")) {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (hunk) {
      oldNo = Number(hunk[1]);
      newNo = Number(hunk[2]);
      inHunk = true;
      out.push({ kind: "hunk", text: line });
    } else if (!inHunk || line.startsWith("diff --git") || line.startsWith("index ") || line.startsWith("--- ") || line.startsWith("+++ ")) {
      inHunk = inHunk && !line.startsWith("diff --git");
      out.push({ kind: "meta", text: line });
    } else if (line.startsWith("+")) {
      out.push({ kind: "add", text: line.slice(1), old: null, new: newNo++ });
    } else if (line.startsWith("-")) {
      out.push({ kind: "del", text: line.slice(1), old: oldNo++, new: null });
    } else if (line.startsWith("\\")) {
      out.push({ kind: "meta", text: line });
    } else {
      out.push({ kind: "ctx", text: line.slice(1), old: oldNo++, new: newNo++ });
    }
  }
  return out;
}

/** The +/− counts a diff carries. */
export function diffStats(lines: readonly DiffLine[]): { add: number; del: number } {
  return {
    add: lines.filter((l) => l.kind === "add").length,
    del: lines.filter((l) => l.kind === "del").length,
  };
}

const TOOL_ICON: Record<string, string> = { Read: "👁", Edit: "✎", MultiEdit: "✎", Write: "✎", Grep: "⌕", Glob: "⌕", Bash: "›_" };

export function DiffView({ diff, truncated }: { diff: string; truncated: boolean }): ReactElement {
  const lines = parseDiff(diff);
  return (
    <div className="scroll-thin overflow-x-auto rounded-md border border-border font-mono text-[11.5px] leading-[18px]">
      <table className="w-full border-collapse">
        <tbody>
          {lines
            .filter((l) => l.kind !== "meta")
            .map((l, i) =>
              l.kind === "hunk" ? (
                <tr key={i} className="bg-info/10 text-info">
                  <td colSpan={3} className="whitespace-pre px-2 py-0.5">
                    {l.text}
                  </td>
                </tr>
              ) : (
                <tr key={i} className={cx(l.kind === "add" && "bg-ok/12", l.kind === "del" && "bg-err/12")}>
                  <td className="w-9 select-none border-r border-border px-1.5 text-right text-faint">{l.old ?? ""}</td>
                  <td className="w-9 select-none border-r border-border px-1.5 text-right text-faint">{l.new ?? ""}</td>
                  <td className={cx("whitespace-pre px-2", l.kind === "add" ? "text-ok" : l.kind === "del" ? "text-err" : "text-muted")}>
                    <span aria-hidden="true" className="select-none">
                      {l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}
                    </span>
                    {l.text}
                  </td>
                </tr>
              ),
            )}
        </tbody>
      </table>
      {truncated && <p className="m-0 border-t border-border px-2 py-1 font-sans text-[11px] text-faint">Diff cut at 200 KB.</p>}
    </div>
  );
}

function ColonyCard({ colony, onOpen }: { colony: MapFileColony; onOpen: (id: string) => void }): ReactElement {
  const [showDiff, setShowDiff] = useState(true);
  const stats = colony.diff ? diffStats(parseDiff(colony.diff)) : null;
  return (
    <article className="rounded-xl border border-border bg-panel-2 p-3">
      <header className="flex items-start gap-2">
        <AntAvatar state={colony.status === "waiting_for_answer" || colony.status === "idle" ? "thinking" : "working"} size={26} ground={false} framed={false} />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-semibold text-text" title={colony.title}>
            {colony.title}
            {colony.issue != null && <span className="ml-1 font-mono text-[11px] font-normal text-faint">#{colony.issue}</span>}
          </div>
          <div className="mt-0.5 flex items-center gap-1.5 text-[11px]">
            <span className="text-muted">{SESSION_STATUS[colony.status]?.label ?? colony.status}</span>
            <span
              className={cx(
                "rounded-full px-1.5 py-px text-[10.5px] font-medium",
                colony.mode === "changing" ? "bg-warn/15 text-warn" : "border border-dashed border-border text-muted",
              )}
            >
              {colony.mode === "changing" ? "changing" : "reading"}
            </span>
            {stats && (
              <span className="font-mono text-[10.5px]">
                <span className="text-ok">+{stats.add}</span> <span className="text-err">−{stats.del}</span>
              </span>
            )}
          </div>
        </div>
        <button
          type="button"
          onClick={() => onOpen(colony.id)}
          className="shrink-0 cursor-pointer rounded-md border border-border bg-panel px-2 py-1 text-[11.5px] text-muted hover:border-border-strong hover:text-text"
        >
          Open colony
        </button>
      </header>
      {colony.activity.length > 0 && (
        <ol className="m-0 mt-2.5 list-none space-y-1 border-l border-border p-0 pl-2.5">
          {colony.activity.map((a, i) => (
            <li key={`${a.ts}-${i}`} className="flex items-baseline gap-1.5 text-[11.5px]">
              <span className="w-12 shrink-0 text-right text-faint tabular-nums">{timeAgo(a.ts)}</span>
              <span aria-hidden="true" className="w-4 shrink-0 text-center font-mono text-[10.5px] text-faint">
                {TOOL_ICON[a.tool] ?? "•"}
              </span>
              <span className="min-w-0 truncate font-mono text-muted" title={a.agent ? `${a.summary} · ${a.agent}` : a.summary}>
                {a.summary}
                {a.agent && <span className="ml-1 font-sans text-faint">· {a.agent}</span>}
              </span>
            </li>
          ))}
        </ol>
      )}
      {colony.diff && (
        <div className="mt-2.5">
          <button
            type="button"
            aria-expanded={showDiff}
            onClick={() => setShowDiff((v) => !v)}
            className="mb-1.5 cursor-pointer border-0 bg-transparent p-0 text-[11.5px] text-muted hover:text-text"
          >
            {showDiff ? "▾" : "▸"} Diff
          </button>
          {showDiff && <DiffView diff={colony.diff} truncated={colony.diff_truncated} />}
        </div>
      )}
    </article>
  );
}

export function FileDetail({
  repo,
  path,
  component,
  onBack,
  onOpenColony,
  initial,
}: {
  repo: string;
  path: string;
  /** The map component the file belongs to, when one does. */
  component: string | null;
  onBack: () => void;
  onOpenColony: (id: string) => void;
  /** Static markup never runs effects: the tests hand the detail in directly. */
  initial?: MapFileDetail;
}): ReactElement {
  const api = useApi();
  const [detail, setDetail] = useState<MapFileDetail | null>(initial ?? null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    const load = () =>
      api.repoMapFile(repo, path).then(
        (d) => {
          if (!alive) return;
          setDetail(d);
          setError(null);
        },
        (e) => alive && setError(errorMessage(e)),
      );
    void load();
    const timer = setInterval(load, POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [api, repo, path]);

  const name = path.split("/").pop() ?? path;
  const shown = detail && detail.path === path ? detail : null;
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-start gap-2 border-b border-border px-3 py-2.5">
        <button type="button" aria-label="back to files" onClick={onBack} className="grid size-6 shrink-0 cursor-pointer place-items-center rounded-md border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
          ←
        </button>
        <div className="min-w-0 flex-1">
          <div className="truncate font-mono text-[12.5px] font-semibold text-text" title={path}>
            {name}
          </div>
          <div className="truncate font-mono text-[10.5px] text-faint" title={path}>
            {path}
            {component ? ` · ${component}` : ""}
          </div>
        </div>
      </div>
      <div className="scroll-thin min-h-0 flex-1 space-y-2.5 overflow-auto p-3">
        {error && !shown ? (
          <p className="m-0 text-[12.5px] text-err">{error}</p>
        ) : !shown ? (
          <p className="m-0 text-[12.5px] text-faint">Loading…</p>
        ) : shown.colonies.length === 0 ? (
          <div className="rounded-xl border border-dashed border-border p-4 text-center">
            <p className="m-0 text-[13px] text-muted">No colony is on this file right now.</p>
            {component && <p className="m-0 mt-1 text-[11.5px] text-faint">It belongs to {component}.</p>}
          </div>
        ) : (
          shown.colonies.map((c) => <ColonyCard key={c.id} colony={c} onOpen={onOpenColony} />)
        )}
      </div>
    </div>
  );
}
