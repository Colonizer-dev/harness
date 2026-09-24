// One message in the conversation: the speaker's mark, the text (Markdown for replies), what was
// attached, and for replies the model, tokens, cost and timing. Actions sit under it: copy, edit
// and resend, regenerate (optionally with another model), branch, and hand-offs to a colony, a
// loop or a GitHub issue. A thumbs-down keeps a private note in this browser.
import { memo, useRef, useState, type ReactElement, type ReactNode } from "react";
import { ProviderMark } from "../../components/providerMark";
import {
  IconAnt,
  IconBranch,
  IconFile,
  IconGitPR,
  IconImage,
  IconMap,
  IconPencil,
  IconRefresh,
  IconRepeat,
  IconSpark,
  IconThumbDown,
} from "../../components/icons";
import { Button, cx } from "../../components/ui";
import { formatCost, formatTokens } from "../../spend";
import type { ChatAttachmentNote, ChatMessage, ChatModels } from "../../types";
import { ChatMarkdown, CopyButton } from "./ChatMarkdown";
import { ModelPopoverList, markFor, shortModel } from "./ModelChip";
import { Popover } from "./Popover";
import { formatMs } from "./logic";

export type MessageAction =
  | { kind: "edit"; content: string }
  | { kind: "regenerate"; model?: string }
  | { kind: "fork" }
  | { kind: "colony" }
  | { kind: "loop" }
  | { kind: "issue" }
  | { kind: "pick" };

const NOTE_KEY = "colonizer.chat.feedback";

function readNotes(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(NOTE_KEY) ?? "{}") as Record<string, string>;
  } catch {
    return {};
  }
}

function writeNote(id: string, note: string | null): void {
  try {
    const notes = readNotes();
    if (note === null) delete notes[id];
    else notes[id] = note;
    localStorage.setItem(NOTE_KEY, JSON.stringify(notes));
  } catch {
    /* storage blocked: the note is lost, nothing else breaks */
  }
}

const KIND_ICON: Record<string, ReactNode> = {
  file: <IconFile size={12} />,
  colony: <IconAnt size={12} />,
  map: <IconMap size={12} />,
  image: <IconImage size={12} />,
  snippet: <IconPencil size={12} />,
  colonies: <IconSpark size={12} />,
  merged: <IconGitPR size={12} />,
};

export function AttachmentPill({ note, onRemove, preview, detail }: { note: ChatAttachmentNote; onRemove?: () => void; preview?: string; detail?: string }): ReactElement {
  return (
    <span className="inline-flex max-w-[260px] items-center gap-1.5 rounded-lg border border-border bg-panel-2/70 py-0.5 pl-1.5 pr-1 text-[11.5px] text-muted">
      {preview ? <img src={preview} alt="" className="size-5 rounded object-cover" /> : <span className="text-faint">{KIND_ICON[note.kind] ?? <IconFile size={12} />}</span>}
      <span className="truncate" title={note.label}>
        {note.label}
      </span>
      {detail && <span className="shrink-0 text-faint">{detail}</span>}
      {onRemove && (
        <button type="button" aria-label={`remove ${note.label}`} onClick={onRemove} className="cursor-pointer rounded border-0 bg-transparent px-1 text-faint hover:text-err">
          ×
        </button>
      )}
    </span>
  );
}

function ActionButton({ label, onClick, children, active, buttonRef }: { label: string; onClick: () => void; children: ReactNode; active?: boolean; buttonRef?: React.Ref<HTMLButtonElement> }): ReactElement {
  return (
    <button
      ref={buttonRef}
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className={cx("grid size-7 cursor-pointer place-items-center rounded-md border-0 bg-transparent hover:bg-panel-2 hover:text-text", active ? "text-warn" : "text-faint")}
    >
      {children}
    </button>
  );
}

function Stats({ m }: { m: ChatMessage }): ReactElement {
  const parts = [
    m.model ? shortModel(m.model) : null,
    m.input_tokens + m.output_tokens > 0 ? `${formatTokens(m.input_tokens)} in · ${formatTokens(m.output_tokens)} out` : null,
    m.cost_usd != null ? formatCost(m.cost_usd) : null,
    m.first_token_ms != null ? `first token ${formatMs(m.first_token_ms)}` : null,
    m.latency_ms != null ? formatMs(m.latency_ms) : null,
    m.stopped ? "stopped" : null,
  ].filter(Boolean);
  return <span className="min-w-0 truncate tabular-nums">{parts.join(" · ")}</span>;
}

export const MessageRow = memo(function MessageRow({
  m,
  models,
  claudeIds,
  isLastReply,
  busy,
  hit,
  onAction,
  onOpenFile,
}: {
  m: ChatMessage;
  models: ChatModels | null;
  claudeIds: readonly { id: string; label: string }[];
  isLastReply: boolean;
  busy: boolean;
  /** Matches the conversation search: outlined. */
  hit: "match" | "current" | null;
  onAction: (m: ChatMessage, action: MessageAction) => void;
  onOpenFile?: (path: string) => void;
}): ReactElement {
  const [editing, setEditing] = useState<string | null>(null);
  const [regenOpen, setRegenOpen] = useState(false);
  const [noteOpen, setNoteOpen] = useState(false);
  const [note, setNote] = useState<string | null>(() => readNotes()[m.id] ?? null);
  const [noteDraft, setNoteDraft] = useState("");
  const regenButton = useRef<HTMLButtonElement>(null);
  const noteButton = useRef<HTMLButtonElement>(null);
  const user = m.role === "user";
  const mark = markFor(m.model ?? "", models);

  return (
    <div
      id={`msg-${m.id}`}
      className={cx(
        "group/msg flex scroll-mt-24 gap-3 rounded-xl px-2 py-2 transition-colors",
        hit === "current" && "bg-warn/10 ring-1 ring-warn/50",
        hit === "match" && "bg-warn/5",
      )}
    >
      <div className="pt-0.5">
        {user ? (
          <span aria-hidden="true" className="grid size-7 place-items-center rounded-full bg-accent-soft text-[11px] font-semibold text-accent">
            You
          </span>
        ) : (
          <span className="grid size-7 place-items-center" title={m.model}>
            <ProviderMark preset={mark.preset} name={mark.name} size="row" />
          </span>
        )}
      </div>
      <div className="min-w-0 flex-1">
        <div className="mb-0.5 flex items-center gap-2 text-[12px]">
          <span className="font-semibold text-text">{user ? "You" : m.model ? shortModel(m.model) : "Assistant"}</span>
          {!user && <span className="text-faint">{mark.name}</span>}
        </div>

        {m.attachments && m.attachments.length > 0 && (
          <div className="mb-1.5 flex flex-wrap gap-1.5">
            {m.attachments.map((a, i) => (
              <AttachmentPill key={i} note={a} />
            ))}
          </div>
        )}

        {editing !== null ? (
          <div className="flex flex-col gap-2">
            <textarea
              value={editing}
              autoFocus
              onChange={(e) => setEditing(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") setEditing(null);
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && editing.trim()) {
                  onAction(m, { kind: "edit", content: editing.trim() });
                  setEditing(null);
                }
              }}
              rows={Math.min(12, Math.max(3, editing.split("\n").length))}
              aria-label="edit message"
              className="scroll-thin w-full resize-y rounded-xl border border-accent bg-panel px-3 py-2 text-[14px] text-text outline-none"
            />
            <div className="flex items-center gap-2">
              <span className="flex-1 text-[11.5px] text-faint">Sends in a new branch; this conversation stays as it is.</span>
              <Button size="sm" onClick={() => setEditing(null)}>
                Cancel
              </Button>
              <Button
                size="sm"
                variant="primary"
                disabled={!editing.trim() || busy}
                onClick={() => {
                  onAction(m, { kind: "edit", content: editing.trim() });
                  setEditing(null);
                }}
              >
                Send in branch
              </Button>
            </div>
          </div>
        ) : user ? (
          <p className="m-0 whitespace-pre-wrap break-words text-[14.5px] leading-[1.65] text-text">{m.content}</p>
        ) : (
          <ChatMarkdown text={m.content || (m.error ? "" : "…")} onOpenFile={onOpenFile} />
        )}
        {m.error && <p className="m-0 mt-1.5 rounded-lg border border-err/30 bg-err/5 px-2.5 py-1.5 text-[12.5px] text-err">{m.error}</p>}
        {note && <p className="m-0 mt-1.5 text-[12px] italic text-warn">Your note: {note}</p>}

        {editing === null && (
          <div className="mt-1 flex min-h-7 items-center gap-0.5 text-[11.5px] text-faint">
            {m.candidate ? (
              <Button size="sm" variant="primary" onClick={() => onAction(m, { kind: "pick" })}>
                Use this reply
              </Button>
            ) : (
              <div className="flex items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover/msg:opacity-100">
                <CopyButton text={m.content} label="Copy message" />
                {user && !busy && (
                  <ActionButton label="Edit and resend" onClick={() => setEditing(m.content)}>
                    <IconPencil size={14} />
                  </ActionButton>
                )}
                {!user && isLastReply && !busy && (
                  <>
                    <ActionButton label="Regenerate" onClick={() => onAction(m, { kind: "regenerate" })}>
                      <IconRefresh size={14} />
                    </ActionButton>
                    <button
                      ref={regenButton}
                      type="button"
                      aria-label="Regenerate with another model"
                      title="Regenerate with another model"
                      onClick={() => setRegenOpen(true)}
                      className="cursor-pointer rounded-md border-0 bg-transparent px-1 text-[11px] text-faint hover:bg-panel-2 hover:text-text"
                    >
                      ▾
                    </button>
                    <Popover open={regenOpen} onClose={() => setRegenOpen(false)} anchor={regenButton} width={380} label="Regenerate with">
                      <ModelPopoverList
                        value={m.model ?? ""}
                        models={models}
                        claudeIds={claudeIds}
                        onPick={(model) => {
                          setRegenOpen(false);
                          onAction(m, { kind: "regenerate", model });
                        }}
                      />
                    </Popover>
                  </>
                )}
                <ActionButton label="Branch from here" onClick={() => onAction(m, { kind: "fork" })}>
                  <IconBranch size={14} />
                </ActionButton>
                {!user && (
                  <>
                    <ActionButton label="Turn into a colony" onClick={() => onAction(m, { kind: "colony" })}>
                      <IconAnt size={14} />
                    </ActionButton>
                    <ActionButton label="Create a loop" onClick={() => onAction(m, { kind: "loop" })}>
                      <IconRepeat size={14} />
                    </ActionButton>
                    <ActionButton label="Create a GitHub issue" onClick={() => onAction(m, { kind: "issue" })}>
                      <IconGitPR size={14} />
                    </ActionButton>
                    <ActionButton
                      label={note ? "Edit your note" : "Not helpful — add a note"}
                      active={Boolean(note)}
                      buttonRef={noteButton}
                      onClick={() => {
                        setNoteDraft(note ?? "");
                        setNoteOpen(true);
                      }}
                    >
                      <IconThumbDown size={14} />
                    </ActionButton>
                    <Popover open={noteOpen} onClose={() => setNoteOpen(false)} anchor={noteButton} width={320} label="Note on this reply">
                      <div className="flex flex-col gap-2 p-3">
                        <span className="text-[12.5px] text-muted">What was wrong? Kept only in this browser.</span>
                        <textarea
                          value={noteDraft}
                          autoFocus
                          onChange={(e) => setNoteDraft(e.target.value)}
                          rows={3}
                          aria-label="note"
                          className="resize-y rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-[13px] text-text outline-none focus:border-accent"
                        />
                        <div className="flex justify-end gap-2">
                          {note && (
                            <Button
                              size="sm"
                              onClick={() => {
                                writeNote(m.id, null);
                                setNote(null);
                                setNoteOpen(false);
                              }}
                            >
                              Clear
                            </Button>
                          )}
                          <Button
                            size="sm"
                            variant="primary"
                            onClick={() => {
                              const n = noteDraft.trim() || "Not helpful";
                              writeNote(m.id, n);
                              setNote(n);
                              setNoteOpen(false);
                            }}
                          >
                            Save
                          </Button>
                        </div>
                      </div>
                    </Popover>
                  </>
                )}
              </div>
            )}
            {!user && (
              <span className="ml-auto flex min-w-0 pl-2">
                <Stats m={m} />
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  );
});

/** A reply still streaming in: the model's mark, the text so far, a caret. */
export function StreamingRow({ model, text, models }: { model: string; text: string; models: ChatModels | null }): ReactElement {
  const mark = markFor(model, models);
  return (
    <div className="flex gap-3 px-2 py-2" aria-live="polite" aria-busy="true">
      <span className="grid size-7 place-items-center pt-0.5">
        <ProviderMark preset={mark.preset} name={mark.name} size="row" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="mb-0.5 text-[12px] font-semibold text-text">{shortModel(model)}</div>
        {text ? (
          <ChatMarkdown text={text} live />
        ) : (
          <span className="chat-dots inline-flex gap-1 py-2" aria-label="thinking">
            <i />
            <i />
            <i />
          </span>
        )}
      </div>
    </div>
  );
}
