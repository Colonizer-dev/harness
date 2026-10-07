// The syntax highlighter, imported lazily so highlight.js (and its colours) stays out of the main
// bundle until a reply actually carries a code block.
type Highlight = typeof import("./highlighter").highlight;

let loading: Promise<Highlight> | null = null;

export function loadHighlighter(): Promise<Highlight> {
  loading ??= import("./highlighter").then((m) => m.highlight);
  return loading;
}
