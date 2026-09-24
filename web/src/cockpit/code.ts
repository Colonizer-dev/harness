// Pure helpers for the Code page: totals across repositories, Monaco language ids, sizes and
// branch-name suggestions. Kept apart from the views so the tests can pin them.
import type { RepoLoc } from "../types";

/** Lines of code per language summed across repositories, biggest first. */
export function sumLoc(locs: readonly (RepoLoc | null | undefined)[]): { total: number; by_language: { name: string; code: number; files: number }[] } {
  const by = new Map<string, { code: number; files: number }>();
  let total = 0;
  for (const loc of locs) {
    if (!loc) continue;
    total += loc.total;
    for (const l of loc.by_language) {
      const e = by.get(l.name) ?? { code: 0, files: 0 };
      e.code += l.code;
      e.files += l.files;
      by.set(l.name, e);
    }
  }
  const by_language = [...by.entries()].map(([name, e]) => ({ name, ...e })).sort((a, b) => b.code - a.code || a.name.localeCompare(b.name));
  return { total, by_language };
}

/** Percent shares for a language bar, rounded to one decimal, from `{name, code}` rows. */
export function shares(rows: readonly { name: string; code: number }[]): { name: string; percent: number }[] {
  const total = rows.reduce((t, r) => t + r.code, 0);
  if (total <= 0) return [];
  return rows.map((r) => ({ name: r.name, percent: Math.round((r.code * 1000) / total) / 10 }));
}

/** 12.3k, 1.2M: a line count for a card. */
export function compact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  if (n >= 1_000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

const MONACO_LANGUAGES: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  go: "go",
  swift: "swift",
  kt: "kotlin",
  kts: "kotlin",
  java: "java",
  rb: "ruby",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  hpp: "cpp",
  cs: "csharp",
  dart: "dart",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  html: "html",
  css: "css",
  scss: "scss",
  md: "markdown",
  mdx: "markdown",
  sql: "sql",
  tf: "hcl",
  hcl: "hcl",
  toml: "ini",
  yml: "yaml",
  yaml: "yaml",
  json: "json",
  xml: "xml",
  proto: "protobuf",
  lua: "lua",
  php: "php",
  ex: "elixir",
  exs: "elixir",
};

/** The Monaco language id for a path; plain text when none fits. */
export function monacoLanguage(path: string): string {
  const file = path.split("/").pop() ?? path;
  if (file === "Dockerfile") return "dockerfile";
  const ext = file.includes(".") ? file.split(".").pop()!.toLowerCase() : "";
  return MONACO_LANGUAGES[ext] ?? "plaintext";
}

/** A branch name for an edit, from its first file: `edit/<file-stem>-<4 chars>`. */
export function suggestBranch(paths: readonly string[], random: () => number = Math.random): string {
  const first = paths[0]?.split("/").pop() ?? "files";
  const stem = first.replace(/\.[^.]+$/, "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 32) || "files";
  const tag = Math.floor(random() * 36 ** 4).toString(36).padStart(4, "0");
  return `edit/${stem}-${tag}`;
}

/** A unified diff of two texts, line by line (LCS), for the Create PR preview. */
export function unifiedDiff(path: string, before: string, after: string): string {
  const a = before.split("\n");
  const b = after.split("\n");
  const n = a.length;
  const m = b.length;
  // A plain LCS table; files over ~4k lines fall back to "replaced".
  if (n * m > 16_000_000) return `--- a/${path}\n+++ b/${path}\n@@ file replaced (${n} → ${m} lines) @@\n`;
  const dp: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const out = [`--- a/${path}`, `+++ b/${path}`];
  let i = 0;
  let j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && a[i] === b[j]) {
      out.push(` ${a[i]}`);
      i++;
      j++;
    } else if (j < m && (i >= n || dp[i][j + 1] >= dp[i + 1][j])) {
      out.push(`+${b[j]}`);
      j++;
    } else {
      out.push(`-${a[i]}`);
      i++;
    }
  }
  return out.join("\n");
}
