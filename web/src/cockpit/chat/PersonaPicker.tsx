// The Chat top bar's persona picker: a button showing the conversation's ant (avatar and name),
// opening a card list of the ants with their role, caste and what they do. Each card can unfold the
// full system prompt. ↑/↓ move, Enter picks, Escape closes; the panel eases in and out.
import { useEffect, useId, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactElement } from "react";
import { IconCheck, IconChevronDown } from "../../components/icons";
import { cx } from "../../components/ui";
import { Popover } from "./Popover";
import { ANT_COLORS, PersonaAnt } from "./PersonaAnt";
import type { Persona } from "./logic";

/** How long the panel takes to ease out; matches `.chat-pop.is-closing` in index.css. */
const CLOSE_MS = 120;

export function PersonaPicker({ personas, value, onPick }: { personas: readonly Persona[]; value: Persona | null; onPick: (p: Persona) => void }): ReactElement {
  const [phase, setPhase] = useState<"closed" | "open" | "closing">("closed");
  const button = useRef<HTMLButtonElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);

  const open = () => {
    if (timer.current) clearTimeout(timer.current);
    setPhase("open");
  };
  const close = () => {
    if (phase !== "open") return;
    setPhase("closing");
    timer.current = setTimeout(() => setPhase("closed"), CLOSE_MS);
  };
  const shown = value ?? personas[0];

  return (
    <>
      <button
        ref={button}
        type="button"
        aria-label="persona"
        aria-haspopup="listbox"
        aria-expanded={phase === "open"}
        title={shown ? `${shown.ant} the ${shown.species.toLowerCase()} · ${shown.name}` : "Persona"}
        onClick={() => (phase === "open" ? close() : open())}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" && phase !== "open") {
            e.preventDefault();
            open();
          }
        }}
        className="persona-ant-host flex min-w-0 cursor-pointer items-center gap-1.5 rounded-full border border-border bg-panel py-0.5 pl-1 pr-2 text-left text-[12.5px] text-text hover:border-border-strong focus-visible:border-accent focus-visible:outline-none"
      >
        {shown && (
          <span className="grid size-6 place-items-center rounded-full" style={tint(shown)}>
            <PersonaAnt persona={shown} size={20} motion="idle" />
          </span>
        )}
        <span className="truncate font-medium">{shown?.ant ?? "Persona"}</span>
        {shown && <span className="hidden truncate text-faint lg:inline">{shown.name}</span>}
        <IconChevronDown size={14} className={cx("shrink-0 text-faint transition-transform duration-150", phase === "open" && "rotate-180")} />
      </button>
      <Popover
        open={phase !== "closed"}
        onClose={close}
        anchor={button}
        placement="bottom-end"
        width={400}
        label="persona"
        className={phase === "closing" ? "is-closing" : undefined}
      >
        <PersonaList
          personas={personas}
          selected={value?.id ?? null}
          onPick={(p) => {
            onPick(p);
            close();
            button.current?.focus();
          }}
        />
      </Popover>
    </>
  );
}

/** A soft wash of the ant's colour behind its avatar. */
const tint = (p: Persona): CSSProperties => ({ background: `color-mix(in srgb, ${ANT_COLORS[p.kind].body} 18%, transparent)` });

/** The ants as cards, for the picker. Exported for the tests. */
export function PersonaList({ personas, selected, onPick }: { personas: readonly Persona[]; selected: string | null; onPick: (p: Persona) => void }): ReactElement {
  const [active, setActive] = useState(() => Math.max(0, personas.findIndex((p) => p.id === selected)));
  const [expanded, setExpanded] = useState<string | null>(null);
  const list = useRef<HTMLDivElement>(null);
  const base = useId();
  useEffect(() => list.current?.focus(), []);

  const onKey = (e: KeyboardEvent) => {
    const n = personas.length;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => (i + (e.key === "ArrowDown" ? 1 : -1) + n) % n);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (personas[active]) onPick(personas[active]);
    } else if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      const p = personas[active];
      if (p?.system) setExpanded(e.key === "ArrowRight" ? p.id : null);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="border-b border-border px-3.5 pb-2 pt-3">
        <div className="text-[13px] font-semibold text-text">Who answers?</div>
        <div className="text-[11.5px] text-faint">Each ant brings its own system prompt to this conversation.</div>
      </div>
      <div
        ref={list}
        role="listbox"
        aria-label="personas"
        tabIndex={0}
        aria-activedescendant={`${base}-${active}`}
        onKeyDown={onKey}
        className="scroll-thin flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-1.5 outline-none"
      >
        {personas.map((p, i) => {
          const isSelected = p.id === selected;
          const open = expanded === p.id;
          return (
            <div
              key={p.id}
              className={cx(
                "persona-row persona-ant-host rounded-xl border transition-colors",
                isSelected ? "border-accent/60 bg-accent-soft/40" : "border-transparent",
                i === active && !isSelected && "bg-panel-2",
                i === active && isSelected && "bg-accent-soft/70",
              )}
              style={{ animationDelay: `${i * 35}ms` }}
              onMouseMove={() => setActive(i)}
            >
              <div
                id={`${base}-${i}`}
                role="option"
                aria-selected={isSelected}
                data-persona={p.id}
                onClick={() => onPick(p)}
                title={p.system || "No system prompt"}
                className="flex cursor-pointer items-center gap-3 px-2.5 py-2"
              >
                <span className={cx("grid size-11 shrink-0 place-items-center rounded-xl", isSelected && "ring-1 ring-accent/70")} style={tint(p)}>
                  <PersonaAnt persona={p} size={40} motion={isSelected ? "idle" : "none"} />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-baseline gap-2">
                    <span className="text-[13.5px] font-semibold text-text">{p.ant}</span>
                    <span className="truncate text-[11px] text-faint">{p.species}</span>
                  </span>
                  <span className="mt-0.5 flex items-center gap-1.5">
                    <span className="rounded-full border border-border px-1.5 text-[10.5px] font-medium uppercase tracking-wide text-muted">{p.name}</span>
                  </span>
                  <span className="mt-1 block text-[12px] leading-snug text-muted">{p.blurb}</span>
                </span>
                {isSelected && <IconCheck size={15} className="shrink-0 self-start text-accent" aria-label="selected" />}
              </div>
              {p.system && (
                <>
                  <button
                    type="button"
                    tabIndex={-1}
                    aria-expanded={open}
                    onClick={() => setExpanded(open ? null : p.id)}
                    className="mb-1.5 ml-[64px] inline-flex cursor-pointer items-center gap-1 rounded border-0 bg-transparent p-0 text-[11px] text-faint hover:text-text"
                  >
                    <IconChevronDown size={12} className={cx("transition-transform duration-150", open ? "rotate-0" : "-rotate-90")} />
                    {open ? "Hide the prompt" : "Show the prompt"}
                  </button>
                  <div className="persona-prompt" data-open={open ? "true" : "false"}>
                    <div>
                      <p className="m-0 mx-2.5 mb-2.5 rounded-lg border border-border bg-bg/60 px-2.5 py-2 font-mono text-[11px] leading-relaxed text-muted">{p.system}</p>
                    </div>
                  </div>
                </>
              )}
            </div>
          );
        })}
      </div>
      <div className="border-t border-border px-3.5 py-2 text-[11px] text-faint">Tweak an ant's prompt under the sliders; save it back to make it stick.</div>
    </div>
  );
}
