// The composer: a centred, rounded box with the attach menu, the model chip (and a second one in
// compare mode), attachment chips, slash commands, an estimate of what the attached context costs,
// and send or stop. Images paste or drop straight in when the model can see them.
import { useEffect, useLayoutEffect, useState, type ReactElement, type ReactNode, type RefObject } from "react";
import { IconCompare, IconSend, IconStop, IconX } from "../../components/icons";
import { cx } from "../../components/ui";
import { formatCost, formatTokens } from "../../spend";
import type { ChatModels } from "../../types";
import type { Pending } from "./AttachMenu";
import { AttachmentPill } from "./Message";
import { ModelChip } from "./ModelChip";
import { slashMatches, type SlashCommand } from "./logic";

export type SendKey = "enter" | "mod-enter";

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
export const MOD = isMac ? "⌘" : "Ctrl";

export function ChatComposer({
  draft,
  onDraft,
  attachments,
  onRemoveAttachment,
  model,
  models,
  claudeIds,
  onModel,
  modelOpen,
  compareModel,
  onCompareModel,
  busy,
  blocked,
  onSubmit,
  onStop,
  sendKey,
  onSendKey,
  attachMenu,
  estimate,
  onImage,
  onSlash,
  textarea,
  hero,
}: {
  draft: string;
  onDraft: (text: string) => void;
  attachments: readonly Pending[];
  onRemoveAttachment: (key: string) => void;
  model: string;
  models: ChatModels | null;
  claudeIds: readonly { id: string; label: string }[];
  onModel: (model: string) => void;
  modelOpen: { current: (() => void) | null };
  compareModel: string | null;
  onCompareModel: (model: string | null) => void;
  busy: boolean;
  blocked: string | null;
  onSubmit: () => void;
  onStop: () => void;
  sendKey: SendKey;
  onSendKey: (key: SendKey) => void;
  attachMenu: ReactNode;
  estimate: { tokens: number; cost: number | null; unknown: boolean } | null;
  onImage: (file: File) => void;
  onSlash: (command: SlashCommand) => void;
  textarea: RefObject<HTMLTextAreaElement | null>;
  hero?: boolean;
}): ReactElement {
  const [slashIndex, setSlashIndex] = useState(0);
  const [dragging, setDragging] = useState(false);
  const commands = slashMatches(draft);
  const showSlash = commands !== null && commands.length > 0;

  useEffect(() => setSlashIndex(0), [draft]);

  // Grow with the text, up to a limit, then scroll.
  useLayoutEffect(() => {
    const t = textarea.current;
    if (!t) return;
    t.style.height = "auto";
    t.style.height = `${Math.min(t.scrollHeight, hero ? 260 : 300)}px`;
  }, [draft, textarea, hero]);

  const canSend = Boolean(draft.trim()) && !blocked && !busy;
  const pickSlash = (cmd: SlashCommand) => {
    onDraft("");
    onSlash(cmd);
  };

  return (
    <div className={cx("mx-auto w-full", hero ? "max-w-[720px]" : "max-w-[760px]")}>
      <div
        onDragOver={(e) => {
          if ([...e.dataTransfer.items].some((i) => i.kind === "file")) {
            e.preventDefault();
            setDragging(true);
          }
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(e) => {
          setDragging(false);
          const files = [...e.dataTransfer.files].filter((f) => f.type.startsWith("image/"));
          if (files.length === 0) return;
          e.preventDefault();
          files.forEach(onImage);
        }}
        className={cx(
          "relative rounded-[22px] border bg-panel shadow-[0_2px_18px_-6px_rgba(0,0,0,0.25)] transition-colors focus-within:border-border-strong",
          dragging ? "border-accent bg-accent-soft/40" : "border-border",
        )}
      >
        {showSlash && (
          <div role="listbox" aria-label="slash commands" className="absolute bottom-full left-3 right-3 mb-2 overflow-hidden rounded-xl border border-border bg-panel p-1 shadow-lg">
            {commands.map((c, i) => (
              <div
                key={c.name}
                role="option"
                aria-selected={i === slashIndex}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pickSlash(c.name);
                }}
                onMouseMove={() => setSlashIndex(i)}
                className={cx("flex cursor-pointer items-baseline gap-3 rounded-lg px-2.5 py-1.5", i === slashIndex && "bg-panel-2")}
              >
                <span className="font-mono text-[13px] text-accent">/{c.name}</span>
                <span className="text-[12px] text-faint">{c.hint}</span>
              </div>
            ))}
          </div>
        )}

        {attachments.length > 0 && (
          <div className="flex flex-wrap gap-1.5 px-3.5 pt-3">
            {attachments.map((p) => (
              <AttachmentPill
                key={p.key}
                note={{ kind: p.attachment.kind === "colonies_today" ? "colonies" : p.attachment.kind === "merged_prs" ? "merged" : p.attachment.kind === "map_component" ? "map" : p.attachment.kind, label: p.label }}
                preview={p.preview}
                detail={p.progress !== undefined ? `uploading ${Math.round(p.progress * 100)}%` : p.chars ? `~${formatTokens(Math.ceil(p.chars / 4))}` : undefined}
                onRemove={() => onRemoveAttachment(p.key)}
              />
            ))}
          </div>
        )}

        <textarea
          ref={textarea}
          value={draft}
          onChange={(e) => onDraft(e.target.value)}
          onPaste={(e) => {
            const images = [...e.clipboardData.files].filter((f) => f.type.startsWith("image/"));
            if (images.length === 0) return;
            e.preventDefault();
            images.forEach(onImage);
          }}
          onKeyDown={(e) => {
            if (showSlash) {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                const n = commands.length;
                setSlashIndex((i) => (i + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
                return;
              }
              if (e.key === "Enter" || e.key === "Tab") {
                e.preventDefault();
                pickSlash(commands[slashIndex].name);
                return;
              }
              if (e.key === "Escape") {
                onDraft("");
                return;
              }
            }
            if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
            const mod = e.metaKey || e.ctrlKey;
            if (mod || (sendKey === "enter" && !e.shiftKey)) {
              e.preventDefault();
              if (canSend) onSubmit();
            }
          }}
          rows={hero ? 2 : 1}
          placeholder={blocked ? "Pick a model you can reach first…" : hero ? "Ask about your code, colonies or plans…  (/ for commands)" : "Reply…  (/ for commands)"}
          aria-label="message"
          className={cx(
            "scroll-thin block w-full resize-none border-0 bg-transparent px-4 text-[15px] leading-relaxed text-text outline-none placeholder:text-faint",
            hero ? "min-h-[64px] pt-4" : "min-h-[48px] pt-3",
          )}
        />

        <div className="flex flex-wrap items-center gap-1.5 px-2.5 pb-2.5 pt-1">
          {attachMenu}
          <ModelChip value={model} models={models} claudeIds={claudeIds} onChange={onModel} openRef={modelOpen} />
          {compareModel !== null ? (
            <span className="inline-flex items-center gap-1">
              <span className="text-[11.5px] text-faint">vs</span>
              <ModelChip value={compareModel} models={models} claudeIds={claudeIds} onChange={onCompareModel} label="second model" exclude={model} compact />
              <button type="button" aria-label="stop comparing" title="Stop comparing" onClick={() => onCompareModel(null)} className="grid size-6 cursor-pointer place-items-center rounded-full border-0 bg-transparent text-faint hover:bg-panel-2 hover:text-text">
                <IconX size={13} />
              </button>
            </span>
          ) : (
            <button
              type="button"
              onClick={() => onCompareModel("")}
              title="Compare two models side by side"
              aria-label="compare two models"
              className="inline-flex cursor-pointer items-center gap-1 rounded-full border-0 bg-transparent px-2 py-1 text-[12px] text-faint hover:bg-panel-2 hover:text-text"
            >
              <IconCompare size={14} /> Compare
            </button>
          )}
          <span className="flex-1" />
          {estimate && (
            <span className="text-[11.5px] tabular-nums text-faint" title="Estimated input for this message's attached context and text (≈ 4 characters per token)">
              ≈ {formatTokens(estimate.tokens)}
              {estimate.unknown ? "+" : ""} tokens{estimate.cost != null ? ` · ${formatCost(estimate.cost)}` : ""}
            </span>
          )}
          <button
            type="button"
            onClick={() => onSendKey(sendKey === "enter" ? "mod-enter" : "enter")}
            title="Which key sends"
            aria-label={`send with ${sendKey === "enter" ? "Enter" : `${MOD}+Enter`}; switch`}
            className="cursor-pointer rounded-md border-0 bg-transparent px-1.5 py-0.5 font-mono text-[10.5px] text-faint hover:bg-panel-2 hover:text-text"
          >
            {sendKey === "enter" ? "⏎ send" : `${MOD}⏎ send`}
          </button>
          {busy ? (
            <button type="button" onClick={onStop} aria-label="stop" title="Stop (Esc)" className="grid size-9 cursor-pointer place-items-center rounded-full border-0 bg-text text-bg hover:opacity-85">
              <IconStop size={14} />
            </button>
          ) : (
            <button
              type="button"
              onClick={onSubmit}
              disabled={!canSend}
              aria-label="send"
              title="Send"
              className="grid size-9 cursor-pointer place-items-center rounded-full border-0 bg-accent text-white transition-opacity hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-35"
            >
              <IconSend size={16} />
            </button>
          )}
        </div>
      </div>
      {blocked && <p className="m-0 mt-2 px-3 text-center text-[12px] text-warn">{blocked}</p>}
    </div>
  );
}
