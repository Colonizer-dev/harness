// Tool calls in a reply (issue #1217): reads that ran, writes held for the user's approval, and what
// became of them. The approval card is the heart of it: the exact action in plain words, the diff
// the API's dry run planned, the blast radius, and Approve / Edit / Reject. Nothing runs without a
// click, and each click settles the call once. Spotlight and the Chat page draw the same card.
import { useCallback, useEffect, useMemo, useState, type ReactElement, type ReactNode } from "react";

import { errorMessage, useApi, useToast } from "../../context";
import { IconAlert, IconCheck, IconPencil, IconSearch, IconX } from "../../components/icons";
import { Spinner, cx } from "../../components/ui";
import type { ChatApproval, ChatDecision, ChatToolNote } from "../../types";

/** The approvals the view knows, and the one way to decide them. */
export interface ApprovalsHandle {
  byId: Record<string, ChatApproval>;
  /** Remembers an approval the stream (or a proposal) delivered. */
  add: (a: ChatApproval) => void;
  /** Settles one: approve, edit or reject. Resolves with the settled approval. */
  decide: (id: string, body: ChatDecision) => Promise<ChatApproval | null>;
  /** Ids being decided right now. */
  busy: ReadonlySet<string>;
}

/** Keeps a conversation's approvals; loads the ones already stored when it opens. */
export function useApprovals(chat: string | null, onSettled?: (a: ChatApproval) => void): ApprovalsHandle {
  const api = useApi();
  const toast = useToast();
  const [byId, setById] = useState<Record<string, ChatApproval>>({});
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set());

  useEffect(() => {
    if (!chat) return;
    let cancelled = false;
    api.chatApprovals(chat).then(
      (r) => !cancelled && setById((cur) => ({ ...cur, ...Object.fromEntries(r.approvals.map((a) => [a.id, a])) })),
      () => {
        /* an older mothership has no approvals; the notes still tell what happened */
      },
    );
    return () => {
      cancelled = true;
    };
  }, [api, chat]);

  const add = useCallback((a: ChatApproval) => setById((cur) => ({ ...cur, [a.id]: a })), []);
  const decide = useCallback(
    async (id: string, body: ChatDecision) => {
      setBusy((b) => new Set(b).add(id));
      try {
        const r = await api.decideApproval(id, body);
        setById((cur) => ({ ...cur, [id]: r.approval }));
        onSettled?.(r.approval);
        if (r.approval.status === "failed") toast({ title: "It was approved, but the call failed", body: r.result, kind: "error" });
        return r.approval;
      } catch (e) {
        toast(errorMessage(e), "error");
        // A 409 means it was settled elsewhere: show where it landed.
        api.chatApprovals(chat ?? undefined).then((r) => setById((cur) => ({ ...cur, ...Object.fromEntries(r.approvals.map((a) => [a.id, a])) })), () => {});
        return null;
      } finally {
        setBusy((b) => {
          const next = new Set(b);
          next.delete(id);
          return next;
        });
      }
    },
    [api, chat, onSettled, toast],
  );
  return useMemo(() => ({ byId, add, decide, busy }), [byId, add, decide, busy]);
}

const KEY_WORDS: Record<string, string> = { subagent_model: "Subagent model", model: "Orchestrator model", background_model: "Background model", small_model: "Small model", summary_model: "Summary model" };

function Plural({ n, one, many }: { n: number; one: string; many: string }): ReactElement {
  return (
    <span className="inline-flex items-baseline gap-1">
      <span className="text-body font-semibold tabular-nums text-text">{n}</span>
      <span className="text-small-lg text-muted">{n === 1 ? one : many}</span>
    </span>
  );
}

/** The held write, with the buttons that settle it. */
export function ApprovalCard({
  approval,
  busy = false,
  onDecide,
  autoFocus = false,
  className,
}: {
  approval: ChatApproval;
  busy?: boolean;
  onDecide: (body: ChatDecision) => void;
  autoFocus?: boolean;
  className?: string;
}): ReactElement {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(() => JSON.stringify(approval.args, null, 2));
  const [problem, setProblem] = useState<string | null>(null);
  const pending = approval.status === "pending";
  const { preview } = approval;
  const reach = preview.blast;

  const tone =
    approval.status === "approved" ? "border-ok/40" : approval.status === "failed" ? "border-err/50" : approval.status === "rejected" ? "border-border" : "border-border-strong";
  const run = (decision: ChatDecision["decision"]) => onDecide({ decision });

  return (
    <section
      aria-label={`approval: ${approval.tool}`}
      data-status={approval.status}
      className={cx(
        "chat-enter relative overflow-hidden rounded-2xl border bg-panel text-text shadow-[0_1px_0_rgb(255_255_255/0.04)_inset,0_8px_28px_rgb(0_0_0/0.14)]",
        tone,
        approval.status === "rejected" && "opacity-70",
        className,
      )}
    >
      <span aria-hidden="true" className={cx("absolute inset-y-0 left-0 w-[3px]", approval.status === "approved" ? "bg-ok" : approval.status === "failed" ? "bg-err" : approval.status === "rejected" ? "bg-faint" : "bg-accent")} />
      <div className="space-y-3 px-4 py-3.5 pl-[18px]">
        <header className="flex items-center gap-2">
          <span
            aria-hidden="true"
            className={cx(
              "grid size-6 place-items-center rounded-lg",
              approval.status === "approved" ? "bg-ok-soft text-ok" : approval.status === "failed" ? "bg-err-soft text-err" : approval.status === "rejected" ? "bg-panel-3 text-muted" : "bg-accent-soft text-accent",
            )}
          >
            {approval.status === "approved" ? <IconCheck size={13} /> : approval.status === "failed" ? <IconAlert size={13} /> : approval.status === "rejected" ? <IconX size={13} /> : <IconPencil size={12} />}
          </span>
          <h3 className="m-0 text-meta-lg font-semibold uppercase tracking-[0.06em] text-muted">
            {pending ? "Needs your approval" : approval.status === "running" ? "Running…" : approval.status === "approved" ? (approval.ran_with ? "Approved with edits" : "Approved") : approval.status === "failed" ? "Approved, but it failed" : "Rejected"}
          </h3>
          <code className="ml-auto truncate rounded-md bg-panel-2 px-1.5 py-0.5 font-mono text-meta-lg text-muted">{approval.tool}</code>
        </header>

        <p className="m-0 text-body leading-snug text-text">{preview.summary}</p>

        {preview.diff.length > 0 && (
          <div role="table" aria-label="what changes" className="overflow-hidden rounded-xl border border-border bg-panel-2/60">
            <div role="row" className="flex items-center justify-between gap-3 border-b border-border px-3 py-1.5 text-meta-lg uppercase tracking-[0.06em] text-faint">
              <span role="columnheader">Setting · before → after</span>
              <span role="columnheader" title={preview.dry_run ? "Planned by the server's dry run" : undefined}>
                {preview.dry_run ? "Dry run" : "Before → after"}
              </span>
            </div>
            {preview.diff.map((row, i) => (
              <div role="row" key={`${row.scope}:${row.target}:${row.key}:${i}`} className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-0.5 border-b border-border/60 px-3 py-2 last:border-b-0">
                <span role="cell" className="text-small-lg text-text">
                  {KEY_WORDS[row.key] ?? row.key}
                  <span className="ml-1.5 text-meta-lg text-faint">{row.scope === "org" ? `org ${row.target}` : "install"}</span>
                </span>
                <span role="cell" className="flex min-w-0 flex-wrap items-center gap-x-2 font-mono text-small">
                  <span className="truncate text-faint line-through decoration-faint/60">{row.was ?? "default"}</span>
                  <span aria-hidden="true" className="text-faint">
                    →
                  </span>
                  <span className="truncate font-medium text-ok">{row.now || "default"}</span>
                </span>
              </div>
            ))}
          </div>
        )}

        {(reach.colonies > 0 || reach.orgs > 0 || reach.repos > 0 || reach.note) && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
            <span className="text-meta-lg uppercase tracking-[0.06em] text-faint">Reaches</span>
            {reach.colonies > 0 && <Plural n={reach.colonies} one="colony" many="colonies" />}
            {reach.orgs > 0 && <Plural n={reach.orgs} one="org" many="orgs" />}
            {reach.repos > 0 && <Plural n={reach.repos} one="repo" many="repos" />}
            {reach.note && <span className="min-w-0 text-small-lg text-muted">{reach.note}</span>}
          </div>
        )}

        {editing && pending && (
          <div className="space-y-2">
            <label className="block text-meta-lg uppercase tracking-[0.06em] text-faint" htmlFor={`edit-${approval.id}`}>
              Arguments
            </label>
            <textarea
              id={`edit-${approval.id}`}
              value={draft}
              spellCheck={false}
              onChange={(e) => {
                setDraft(e.target.value);
                setProblem(null);
              }}
              rows={Math.min(10, draft.split("\n").length + 1)}
              className="w-full resize-y rounded-xl border border-border bg-panel-2 p-2.5 font-mono text-small leading-[1.5] text-text outline-none focus:border-accent"
            />
            {problem && (
              <p role="alert" className="m-0 text-small-lg text-err">
                {problem}
              </p>
            )}
            <div className="flex gap-2">
              <button
                type="button"
                disabled={busy}
                onClick={() => {
                  try {
                    const args = JSON.parse(draft) as unknown;
                    if (!args || typeof args !== "object" || Array.isArray(args)) throw new Error("The arguments are an object.");
                    onDecide({ decision: "edit", args: args as Record<string, unknown> });
                  } catch (e) {
                    setProblem(e instanceof SyntaxError ? "That is not valid JSON." : errorMessage(e));
                  }
                }}
                className="h-8 cursor-pointer rounded-lg border-0 bg-accent px-3 text-small-lg font-semibold text-on-accent hover:brightness-110 disabled:opacity-50"
              >
                Approve with these
              </button>
              <button type="button" onClick={() => setEditing(false)} className="h-8 cursor-pointer rounded-lg border border-border bg-transparent px-3 text-small-lg text-muted hover:text-text">
                Cancel
              </button>
            </div>
          </div>
        )}

        {pending && !editing && (
          <div className="flex flex-wrap items-center gap-2 pt-0.5">
            <button
              type="button"
              autoFocus={autoFocus}
              disabled={busy}
              onClick={() => run("approve")}
              className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg border-0 bg-accent px-3.5 text-small-lg font-semibold text-on-accent transition-[filter,transform] hover:brightness-110 active:scale-[0.98] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-50"
            >
              {busy ? <Spinner className="size-3" /> : <IconCheck size={13} />} Approve
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => setEditing(true)}
              className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-transparent px-3 text-small-lg text-text hover:bg-panel-2 disabled:opacity-50"
            >
              <IconPencil size={12} /> Edit
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => run("reject")}
              className="inline-flex h-8 cursor-pointer items-center rounded-lg border-0 bg-transparent px-3 text-small-lg text-muted hover:bg-panel-2 hover:text-err disabled:opacity-50"
            >
              Reject
            </button>
            <span className="ml-auto text-meta-lg text-faint">Nothing runs until you approve.</span>
          </div>
        )}

        {approval.status === "running" && (
          <p className="m-0 inline-flex items-center gap-2 text-small-lg text-muted">
            <Spinner className="size-3" /> Running…
          </p>
        )}
        {(approval.status === "approved" || approval.status === "failed") && approval.result && (
          <p className={cx("m-0 text-small-lg", approval.status === "failed" ? "text-err" : "text-muted")}>{approval.result}</p>
        )}
      </div>
    </section>
  );
}

/** Several held writes at once: each listed, and one button to approve them all. */
function BatchBar({ pending, busy, onApproveAll }: { pending: ChatApproval[]; busy: boolean; onApproveAll: () => void }): ReactElement {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-xl border border-border bg-panel-2/60 px-3 py-2">
      <span className="text-small-lg text-text">{pending.length} changes are waiting</span>
      <span className="min-w-0 truncate text-small-lg text-muted">{pending.map((a) => a.tool.replace(/_/g, " ")).join(" · ")}</span>
      <button
        type="button"
        disabled={busy}
        onClick={onApproveAll}
        className="ml-auto h-7 cursor-pointer rounded-lg border-0 bg-accent px-3 text-small-lg font-semibold text-on-accent hover:brightness-110 disabled:opacity-50"
      >
        Approve all {pending.length}
      </button>
    </div>
  );
}

function ReadNote({ note }: { note: ChatToolNote }): ReactElement {
  const failed = note.status === "failed" || note.status === "refused";
  return (
    <details className="group/read">
      <summary
        className={cx(
          "inline-flex max-w-full cursor-pointer list-none items-center gap-1.5 rounded-full border px-2.5 py-1 text-small-lg [&::-webkit-details-marker]:hidden",
          failed ? "border-warn/40 bg-warn-soft text-warn" : "border-border bg-panel-2/60 text-muted hover:text-text",
        )}
      >
        {failed ? <IconAlert size={12} /> : <IconSearch size={12} />}
        <span className="truncate">{note.summary}</span>
        {!failed && <span className="text-meta-lg text-faint">read only</span>}
      </summary>
      {note.result && <pre className="m-0 mt-1.5 max-h-40 overflow-auto whitespace-pre-wrap rounded-lg bg-panel-2 p-2 font-mono text-small text-muted">{note.result}</pre>}
    </details>
  );
}

/** A reply's tool calls, in order. */
export function ToolCalls({
  notes,
  approvals,
  autoFocusFirst = false,
}: {
  notes: readonly ChatToolNote[];
  approvals: ApprovalsHandle;
  autoFocusFirst?: boolean;
}): ReactElement | null {
  const known = notes.flatMap((n) => (n.approval ? [approvals.byId[n.approval]].filter((a): a is ChatApproval => Boolean(a)) : []));
  const pending = known.filter((a) => a.status === "pending");
  if (notes.length === 0) return null;
  let focused = false;
  const rows: ReactNode[] = notes.map((n, i) => {
    const a = n.approval ? approvals.byId[n.approval] : undefined;
    if (n.kind === "write" && a) {
      const first = autoFocusFirst && !focused && a.status === "pending";
      if (first) focused = true;
      return (
        <ApprovalCard
          key={n.approval}
          approval={a}
          busy={approvals.busy.has(a.id)}
          autoFocus={first}
          onDecide={(body) => void approvals.decide(a.id, body)}
        />
      );
    }
    if (n.kind === "write") {
      // Not loaded yet (or an older mothership): say where it stands from the note alone.
      return (
        <p key={`${n.tool}:${i}`} className={cx("m-0 rounded-xl border px-3 py-2 text-small-lg", n.status === "rejected" ? "border-border text-muted" : n.status === "refused" ? "border-warn/40 bg-warn-soft text-warn" : "border-border bg-panel-2/60 text-text")}>
          <span className="font-medium">{n.status === "pending" ? "Waiting for your approval" : n.status === "approved" ? "Approved" : n.status === "rejected" ? "Rejected" : n.status === "refused" ? "Refused" : n.status}: </span>
          {n.summary}
          {n.result ? <span className="text-muted"> — {n.result}</span> : null}
        </p>
      );
    }
    return <ReadNote key={`${n.tool}:${i}`} note={n} />;
  });
  return (
    <div className="mt-2 flex flex-col gap-2" data-testid="tool-calls">
      {pending.length > 1 && (
        <BatchBar
          pending={pending}
          busy={pending.some((a) => approvals.busy.has(a.id))}
          onApproveAll={() =>
            void (async () => {
              for (const a of pending) await approvals.decide(a.id, { decision: "approve" });
            })()
          }
        />
      )}
      {rows}
    </div>
  );
}
