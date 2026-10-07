// Monaco, loaded only when the Code editor opens (this module is imported lazily): the editor, or
// its diff view, with blame in the gutter. Syntax colouring comes from Monaco's Monarch grammars;
// there are no language services, so one editor worker is all it needs.
import { useEffect, useRef, type ReactElement } from "react";
import * as monaco from "monaco-editor/editor/editor.api";
import "monaco-editor/basic-languages/monaco.contribution";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";
import type { RepoBlame } from "../types";

(self as unknown as { MonacoEnvironment: unknown }).MonacoEnvironment = { getWorker: () => new EditorWorker() };

/** Dark when the cockpit is: an explicit data-theme wins, else the system's preference. */
function isDark(): boolean {
  const theme = document.documentElement.dataset.theme;
  if (theme === "dark") return true;
  if (theme === "light") return false;
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
}

function useTheme(): void {
  useEffect(() => {
    const apply = () => monaco.editor.setTheme(isDark() ? "vs-dark" : "vs");
    apply();
    const observer = new MutationObserver(apply);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    media?.addEventListener?.("change", apply);
    return () => {
      observer.disconnect();
      media?.removeEventListener?.("change", apply);
    };
  }, []);
}

export interface MonacoPaneProps {
  /** The model key: one model per open file (and ref). */
  uri: string;
  value: string;
  language: string;
  readOnly?: boolean;
  onChange?: (value: string) => void;
  onSelection?: (range: [number, number] | null) => void;
  /** Show this as the original in a side-by-side diff instead of the plain editor. */
  diffOriginal?: string | null;
  blame?: RepoBlame | null;
}

export default function MonacoPane({ uri, value, language, readOnly, onChange, onSelection, diffOriginal, blame }: MonacoPaneProps): ReactElement {
  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const diff = useRef<monaco.editor.IStandaloneDiffEditor | null>(null);
  const decorations = useRef<monaco.editor.IEditorDecorationsCollection | null>(null);
  const change = useRef(onChange);
  const select = useRef(onSelection);
  change.current = onChange;
  select.current = onSelection;
  useTheme();

  const isDiff = diffOriginal != null;
  useEffect(() => {
    if (!host.current) return;
    const common = { automaticLayout: true, minimap: { enabled: false }, fontSize: 13, scrollBeyondLastLine: false, readOnly: !!readOnly };
    const model = monaco.editor.getModel(monaco.Uri.parse(uri)) ?? monaco.editor.createModel(value, language, monaco.Uri.parse(uri));
    if (isDiff) {
      const d = monaco.editor.createDiffEditor(host.current, { ...common, renderSideBySide: host.current.clientWidth > 900, originalEditable: false });
      const original = monaco.editor.createModel(diffOriginal ?? "", language);
      d.setModel({ original, modified: model });
      const sub = d.getModifiedEditor().onDidChangeModelContent(() => change.current?.(model.getValue()));
      diff.current = d;
      return () => {
        sub.dispose();
        d.dispose();
        original.dispose();
        diff.current = null;
      };
    }
    const e = monaco.editor.create(host.current, { ...common, model, glyphMargin: !!blame });
    const sub = e.onDidChangeModelContent(() => change.current?.(model.getValue()));
    const sel = e.onDidChangeCursorSelection(({ selection }: monaco.editor.ICursorSelectionChangedEvent) => {
      select.current?.(selection.isEmpty() ? null : [selection.startLineNumber, selection.endLineNumber]);
    });
    editor.current = e;
    decorations.current = e.createDecorationsCollection();
    return () => {
      sub.dispose();
      sel.dispose();
      e.dispose();
      editor.current = null;
    };
    // The editor is rebuilt only when the file or the view kind changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [uri, isDiff]);

  // Outside changes (a restored draft, a reverted tab) reach the model without resetting the cursor.
  useEffect(() => {
    const model = monaco.editor.getModel(monaco.Uri.parse(uri));
    if (model && model.getValue() !== value) model.setValue(value);
  }, [uri, value]);

  useEffect(() => {
    editor.current?.updateOptions({ readOnly: !!readOnly });
  }, [readOnly]);

  // Blame: each line's author and date in the gutter, as a hover and an inline label.
  useEffect(() => {
    const e = editor.current;
    if (!e || !decorations.current) return;
    e.updateOptions({ glyphMargin: !!blame, lineDecorationsWidth: blame ? 180 : 10 });
    if (!blame) {
      decorations.current.clear();
      return;
    }
    const items: monaco.editor.IModelDeltaDecoration[] = [];
    let previous = "";
    blame.lines.forEach((sha, i) => {
      const c = blame.commits[sha] ?? {};
      const label = sha === previous ? "" : `${(c.author ?? "?").slice(0, 14)} · ${c.time ? new Date(c.time * 1000).toISOString().slice(0, 10) : ""}`;
      previous = sha;
      items.push({
        range: new monaco.Range(i + 1, 1, i + 1, 1),
        options: {
          isWholeLine: true,
          before: label ? { content: `${label.padEnd(28)} `, inlineClassName: "blame-label" } : undefined,
          hoverMessage: { value: `**${c.author ?? "?"}** · ${sha.slice(0, 7)}\n\n${c.summary ?? ""}` },
        },
      });
    });
    decorations.current.set(items);
  }, [blame, uri]);

  return <div ref={host} className="h-full min-h-0 w-full" />;
}

/** Drops every Monaco model whose uri starts with `prefix` (a closed repository or ref). */
export function disposeModels(prefix: string): void {
  for (const m of monaco.editor.getModels()) if (m.uri.toString().startsWith(prefix)) m.dispose();
}
