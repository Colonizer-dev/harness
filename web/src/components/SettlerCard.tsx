// A settler — a subagent — as one card in the colony chat. The ant performs; everything else stays still.
// Motion for the card (status-in, .settler-trail, .settler-frame) lives in index.css next to the `.ant` rules.
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { AntAvatar, antPhase, type AntActivity, type AntRole, type AntState } from "./AntAvatar";
import { IconChevron } from "./icons";
import { MarkdownBlock } from "./Markdown";
import { cx } from "./ui";

export interface SettlerStep {
  /** The step in plain words: "Reading session.ts". */
  label: string;
  /** What it acted on: 'rg -n "guest" src/', a path, a pattern. */
  detail: string;
  failed?: boolean;
  running?: boolean;
}

/** True for 1.2 s each time `errors` grows: long enough for the ant's stumble to play. */
export function useStumble(errors: number): boolean {
  const seen = useRef(errors);
  const [stumbling, setStumbling] = useState(false);
  useEffect(() => {
    if (errors <= seen.current) {
      seen.current = errors;
      return;
    }
    seen.current = errors;
    setStumbling(true);
    const timer = setTimeout(() => setStumbling(false), 1200);
    return () => clearTimeout(timer);
  }, [errors]);
  return stumbling;
}

/** The first sentence of a markdown report, as plain text, for the collapsed card. */
function firstSentence(report: string): string {
  const line = report
    .split("\n")
    .map((l) => l.replace(/^\s*(#+|[-*>]|\d+\.)\s+/, "").trim())
    .find(Boolean);
  if (!line) return "";
  const plain = line.replace(/[*_`]/g, "").replace(/\[([^\]]*)\]\([^)]*\)/g, "$1");
  return plain.split(/(?<=[.!?])\s/)[0] ?? plain;
}

export function SettlerCard({
  state,
  role,
  activity = "run",
  error = false,
  name,
  task,
  status,
  steps,
  report = "",
  stepList = [],
  phase = 0,
  defaultOpen = false,
  children,
}: {
  state: AntState;
  role: AntRole;
  activity?: AntActivity;
  error?: boolean;
  name: string;
  task?: string;
  status: string;
  steps: number;
  report?: string;
  stepList?: SettlerStep[];
  /** Its place in a crew of settlers shown together; staggers the loops. */
  phase?: number;
  defaultOpen?: boolean;
  /** The full transcript, rendered under the steps when open. */
  children?: ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const [stepsOpen, setStepsOpen] = useState(state !== "done");
  const bodyId = useId();
  const summary = state === "done" ? firstSentence(report) : "";
  const stepsLabel = steps === 1 ? "1 step" : `${steps} steps`;
  return (
    <div className="relative overflow-hidden rounded-[10px] border border-border bg-panel text-text" style={antPhase(phase)}>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-controls={bodyId}
        className="flex w-full cursor-pointer items-start gap-3 rounded-[10px] px-3 pb-3 pt-2.5 text-left hover:bg-panel-2"
      >
        <span className="mt-px rounded-lg border border-border">
          <AntAvatar state={state} role={role} activity={activity} error={error} phase={phase} />
        </span>
        <span className="flex min-w-0 flex-1 flex-col gap-px">
          <span className="flex min-w-0 flex-wrap items-baseline gap-x-2">
            <span className="whitespace-nowrap text-[13px] font-semibold text-accent">{name}</span>
            {task && <span className="min-w-0 flex-1 truncate text-[12.5px] text-muted">{task}</span>}
          </span>
          {/* Re-keyed on the text so a new status eases in. */}
          <span className="block min-h-[19px] truncate font-mono text-[12px] text-muted">
            <span key={status} className="status-in inline-block max-w-full truncate align-top">
              {status}
            </span>
          </span>
          {!open && summary && <span className="mt-0.5 block truncate text-[13px] text-text">{summary}</span>}
        </span>
        <span className="mt-0.5 flex shrink-0 items-center gap-2">
          {steps > 0 && (
            <span className="rounded border border-border bg-panel-2 px-1.5 font-mono text-[11.5px] text-muted">{stepsLabel}</span>
          )}
          <span className="flex items-center gap-1 text-[12px] text-muted">
            {open ? "Hide work" : "Show work"}
            <IconChevron size={13} className={cx("transition-transform", open && "rotate-90")} />
          </span>
        </span>
      </button>
      <svg className="settler-trail" data-s={state} aria-hidden="true">
        <line x1="0" y1="1" x2="100%" y2="1" />
      </svg>
      {open && (
        <div id={bodyId} className="flex flex-col gap-3 border-t border-border px-3.5 pb-3.5 pt-3">
          {report && (
            <div className="rounded-lg border border-border bg-panel-2 px-3.5 py-3">
              <div className="mb-1.5 flex items-center gap-2">
                <span className="font-mono text-[11px] font-semibold uppercase tracking-[0.08em] text-muted">
                  {state === "done" ? "Report" : "Report so far"}
                </span>
                {state === "writing" && <span className="font-mono text-[11px] text-accent">streaming…</span>}
              </div>
              <MarkdownBlock className="leading-[1.55] text-text [text-wrap:pretty]">{report}</MarkdownBlock>
            </div>
          )}
          {steps > 0 && (
            <div>
              <button
                type="button"
                onClick={() => setStepsOpen((o) => !o)}
                aria-expanded={stepsOpen}
                className="flex cursor-pointer items-center gap-1.5 font-mono text-[12px] text-muted hover:text-text"
              >
                <IconChevron size={12} className={cx("transition-transform", stepsOpen && "rotate-90")} />
                {stepsOpen ? "Hide" : "Show"} {stepsLabel}
              </button>
              {stepsOpen && stepList.length > 0 && (
                <ol className="ml-[5px] mt-2 flex flex-col gap-1.5 border-l border-border pl-3.5">
                  {stepList.map((s, i) => (
                    <li key={i} className="relative flex min-w-0 items-baseline gap-2 text-[12.5px]">
                      <span
                        className={cx(
                          "absolute -left-[17.5px] top-1.5 size-1.5 rounded-full",
                          s.failed ? "bg-warn" : s.running ? "bg-accent" : "bg-ok",
                        )}
                      />
                      <span className="whitespace-nowrap text-text">{s.label}</span>
                      {s.detail && <span className="min-w-0 truncate font-mono text-[11.5px] text-muted">{s.detail}</span>}
                      {s.failed && <span className="whitespace-nowrap text-[11.5px] text-warn">didn't work</span>}
                    </li>
                  ))}
                </ol>
              )}
              {stepsOpen && children && <div className="mt-2 space-y-1.5">{children}</div>}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
