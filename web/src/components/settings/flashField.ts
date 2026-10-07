// Landing on a setting from the search (issue #1180): find the text of its label inside the page,
// scroll it to the middle and give it a short accent flash. Labels are matched by their text, so no
// pane has to name its fields for the search to reach them, and a provider or module field added
// later is found the same way.

/** Lower-case words joined by dashes: what a field's address `#hash` carries. */
export function slugify(text: string): string {
  return text
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

const FLASH = "setting-flash";

/**
 * Finds the element whose own text is `label` (or, failing that, starts with it) inside `root`,
 * returning the row around it when there is one. Null when the page has no such text — a field
 * hidden under an "Advanced" fold, say.
 */
export function findField(root: ParentNode, label: string): HTMLElement | null {
  const want = slugify(label);
  if (!want) return null;
  const walker = document.createTreeWalker(root as Node, NodeFilter.SHOW_TEXT);
  let loose: HTMLElement | null = null;
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const text = node.textContent ?? "";
    if (!text.trim()) continue;
    const parent = node.parentElement;
    if (!parent || parent.closest("[data-nav], [aria-hidden='true'], script, style")) continue;
    const slug = slugify(text);
    if (slug === want) return rowAround(parent);
    if (!loose && slug.startsWith(want)) loose = parent;
  }
  return loose ? rowAround(loose) : null;
}

/** The row or card a label sits in, so the flash frames the whole setting rather than its words. */
function rowAround(el: HTMLElement): HTMLElement {
  return el.closest<HTMLElement>("[data-setting], .py-3\\.5, .rounded-xl, .rounded-2xl, li, tr") ?? el;
}

/** Scrolls the field into the middle and flashes it. True when it was found. */
export function flashField(root: ParentNode, label: string): boolean {
  const el = findField(root, label);
  if (!el) return false;
  const reduce = typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  el.scrollIntoView({ block: "center", behavior: reduce ? "auto" : "smooth" });
  el.classList.remove(FLASH);
  void el.offsetWidth; // restart the animation when the same field is picked twice
  el.classList.add(FLASH);
  window.setTimeout(() => el.classList.remove(FLASH), 2600);
  return true;
}
