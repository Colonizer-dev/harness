// Loaded on demand (see highlight.ts): highlight.js's common languages and the token colours. Kept
// out of the main bundle; the first fenced code block in a reply pulls this chunk in.
import hljs from "highlight.js/lib/common";
import "./highlight.css";

/** The block as highlighted HTML (highlight.js escapes the source), and the language it used. */
export function highlight(code: string, lang: string | undefined): { html: string; language: string | null } {
  const known = lang && hljs.getLanguage(lang) ? lang : null;
  if (known) return { html: hljs.highlight(code, { language: known, ignoreIllegals: true }).value, language: known };
  if (code.length > 20_000) return { html: "", language: null };
  const auto = hljs.highlightAuto(code);
  return { html: auto.value, language: auto.language ?? null };
}
