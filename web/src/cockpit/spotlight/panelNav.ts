// The keys every Spotlight panel shares (issue #1228), as plain functions so one set of tests pins
// them for the model menu, Colonize, the workspace switcher, the notifications and Spotlight itself.
// ↑ and ↓ move the selection and wrap, ⇥ (⇧⇥) jumps to the next (previous) section, ↵ picks the
// selected row, and a modified or composing ↵ is left to the caller.

export type NavAction = { type: "move"; delta: 1 | -1 } | { type: "section"; back: boolean } | { type: "pick" };

export interface NavKey {
  key: string;
  shiftKey: boolean;
  isComposing?: boolean;
}

/** What a key press means to a panel with `rows` selectable rows in `sections` sections; null is not ours. */
export function navAction(e: NavKey, rows: number, sections: number, hasSelection: boolean): NavAction | null {
  if (e.isComposing) return null;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") return rows > 0 ? { type: "move", delta: e.key === "ArrowDown" ? 1 : -1 } : null;
  if (e.key === "Tab") return rows > 0 && sections > 1 ? { type: "section", back: e.shiftKey } : null;
  if (e.key === "Enter" && !e.shiftKey && hasSelection) return { type: "pick" };
  return null;
}

/** The index after a move: wraps, and from "nothing selected" (-1) the first row going down, the last going up. */
export function stepIndex(at: number, count: number, delta: number): number {
  if (count === 0) return -1;
  if (at < 0) return delta > 0 ? 0 : count - 1;
  return (at + delta + count) % count;
}

/** The index of the first row of the next (or previous) section; `sectionOf` names each row's section. */
export function sectionIndex(sectionOf: readonly string[], at: number, back: boolean): number {
  const ids = [...new Set(sectionOf)];
  if (ids.length < 2) return Math.max(at, 0);
  const here = ids.indexOf(sectionOf[at] ?? ids[0]);
  const next = ids[(here + (back ? -1 : 1) + ids.length) % ids.length];
  return sectionOf.indexOf(next);
}

/** Where the selection starts: the primary row, else the current choice, else the first (or none). */
export function startIndex(rows: readonly { primary?: boolean; checked?: boolean }[], autoSelect: boolean): number {
  const primary = rows.findIndex((r) => r.primary);
  if (primary >= 0) return primary;
  if (!autoSelect) return -1;
  const checked = rows.findIndex((r) => r.checked);
  return checked >= 0 ? checked : rows.length > 0 ? 0 : -1;
}
