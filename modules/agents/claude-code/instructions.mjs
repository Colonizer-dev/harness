/**
 * Conditional instructions (issue #473): `FOOTGUNS.md` / subdirectory `AGENTS.md` files and
 * `.colonizer/instructions.toml` rules, injected by the runner's hooks while their condition holds —
 * once per fragment per context window, re-injected after a compaction, dropped when it no longer
 * does. Like the denial layer, this only ever adds context: no permission decision.
 */

import { readFileSync, realpathSync, statSync } from 'node:fs';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';

/** The directory file, and the subdirectory one (a repo root AGENTS.md already loads as project instructions). */
export const FOOTGUNS_BASENAME = 'FOOTGUNS.md';
export const AGENTS_BASENAME = 'AGENTS.md';

/** Repo config mapping conditions to instruction files, beside `.colonizer/sensitivity.toml`. */
export const INSTRUCTIONS_TOML = '.colonizer/instructions.toml';

/** One fragment's budget; a longer file is cut with a marker rather than flooding the window. */
export const MAX_FRAGMENT_BYTES = 16 * 1024;

/** Recently touched paths remembered per window; conditions are matched against these. */
export const MAX_RECENT_PATHS = 20;

/** Bash/prompt tokens examined per hook call: a cheap heuristic must stay cheap. */
export const MAX_TOKENS_CHECKED = 64;

/** Tools whose input names a path, as the PreToolUse matcher string. */
export const PATH_TOOLS_MATCHER = '^(Read|Edit|Write|MultiEdit|NotebookEdit|Glob|Grep|Bash)$';

const escapeRegExp = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const globCache = new Map();

/** gitignore-style glob → RegExp: `**` crosses separators (with a trailing slash it also matches at the root), `*` and `?` do not; anchored. */
export function globToRegExp(glob) {
  let re = globCache.get(glob);
  if (re) return re;
  let out = '';
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === '*') {
      if (glob[i + 1] === '*') {
        i++;
        if (glob[i + 1] === '/') {
          i++; // `**/` also matches at the root, like gitignore's `**/foo`
          out += '(?:.*/)?';
        } else {
          while (glob[i + 1] === '*') i++;
          out += '.*';
        }
      } else out += '[^/]*';
    } else if (c === '?') out += '[^/]';
    else out += escapeRegExp(c);
  }
  re = new RegExp(`^${out}$`);
  globCache.set(glob, re);
  return re;
}

/**
 * Whether a workspace-relative path matches a rule glob, gitignore-style: the pattern holds for the
 * path itself or any of its ancestor directories, so `web/*` and `web` cover descendants (`web/src`
 * matches `web/*`, which drags `web/src/App.tsx` in with it), and `dir/**` also covers `dir` itself.
 * A pattern without `/` is matched against the basename at any level, so `*.tsx` is a file type and
 * `docs` covers what is inside docs.
 * @param {string} glob
 * @param {string} relPath
 */
export function globMatches(glob, relPath) {
  if (typeof glob !== 'string' || typeof relPath !== 'string' || !glob || !relPath) return false;
  const trimmed = glob.replace(/\/+$/, '');
  const re = globToRegExp(trimmed);
  if (!trimmed.includes('/')) return relPath.split('/').some((part) => re.test(part));
  const dir = trimmed.endsWith('/**') ? trimmed.slice(0, -3) : null;
  let prefix = '';
  for (const part of relPath.split('/')) {
    prefix = prefix ? `${prefix}/${part}` : part;
    if (prefix === dir || re.test(prefix)) return true;
  }
  return false;
}

/**
 * A workspace-relative path inside `workspace`, or null for anything else — the whole check a
 * touched path has to pass before it is remembered or can match a condition.
 * @param {string} workspace
 * @param {string} raw
 * @returns {string | null}
 */
export function relInside(workspace, raw) {
  if (typeof raw !== 'string' || !raw.trim()) return null;
  const base = resolve(workspace);
  const rel = relative(base, resolve(base, raw));
  if (!rel || rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) return null;
  return rel.split(sep).join('/');
}

function defaultExists(path) {
  try {
    return statSync(path, { throwIfNoEntry: false }) !== undefined;
  } catch {
    return false;
  }
}

/** Whitespace-separated tokens of a command or prompt that resolve to something in the workspace; deliberately crude, because a miss only costs a fragment a turn later and a hit must never crash. */
export function tokensFromText(text, { workspace = process.cwd(), exists = defaultExists, maxTokens = MAX_TOKENS_CHECKED } = {}) {
  const tokens = String(text ?? '').split(/\s+/).slice(0, maxTokens);
  const found = [];
  for (const token of tokens) {
    const cleaned = token.replace(/^['"`({]+/, '').replace(/[)'"},.;:!?]+$/, '');
    if (!cleaned || cleaned.startsWith('-')) continue;
    try {
      if (exists(resolve(workspace, cleaned))) found.push(cleaned);
    } catch {
      // an unreadable token is not a path
    }
  }
  return found;
}

const TOOL_PATH_FIELD = {
  Read: 'file_path',
  Edit: 'file_path',
  Write: 'file_path',
  MultiEdit: 'file_path',
  NotebookEdit: 'notebook_path',
  Glob: 'path',
  Grep: 'path',
};

/** The raw paths a tool call touches: its path field, or for Bash the tokens naming workspace files. */
export function pathsFromToolInput(toolName, toolInput = {}, opts = {}) {
  const input = toolInput && typeof toolInput === 'object' ? toolInput : {};
  if (toolName === 'Bash') return tokensFromText(input.command, opts);
  const value = TOOL_PATH_FIELD[toolName] && input[TOOL_PATH_FIELD[toolName]];
  return typeof value === 'string' && value.trim() ? [value.trim()] : [];
}

const decodeTomlEscapes = (text, n) =>
  text.replace(/\\(.)/g, (_, esc) => {
    if (esc === 'n') return '\n';
    if (esc === 't') return '\t';
    if (esc === '"' || esc === '\\') return esc;
    throw new Error(`line ${n}: unsupported escape \\${esc}`);
  });

const fail = (n, why) => {
  throw new Error(`line ${n}: ${why}`);
};

function stripComment(line, n) {
  let inString = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '\\' && inString) i++;
    else if (c === '"') inString = !inString;
    else if (c === '#' && !inString) return line.slice(0, i);
  }
  if (inString) fail(n, 'unterminated string');
  return line;
}

function parseString(text, n) {
  const match = text.match(/^"((?:[^"\\]|\\.)*)"\s*(.*)$/);
  if (!match || match[2]) fail(n, 'trailing characters after string');
  return decodeTomlEscapes(match[1], n);
}

function parseArray(text, n) {
  const inner = text.match(/^\[(.*)\]\s*$/)?.[1];
  if (inner === undefined) fail(n, 'unterminated array');
  const items = [];
  let rest = inner.trim();
  while (rest) {
    const item = rest.match(/^"((?:[^"\\]|\\.)*)"\s*(,?)\s*(.*)$/);
    if (!item) fail(n, 'array items must be "strings"');
    items.push(decodeTomlEscapes(item[1], n));
    if (!item[2] && item[3]) fail(n, 'expected , between array items');
    rest = item[3];
  }
  return items;
}

function assign(rule, key, value, n) {
  if (key === 'file') {
    if (typeof value !== 'string') fail(n, 'file must be a "string"');
    rule.file = value;
  } else if (key === 'paths' || key === 'labels') {
    if (!Array.isArray(value) || value.some((v) => typeof v !== 'string')) {
      fail(n, `${key} must be an array of "strings"`);
    }
    rule[key] = value;
  } else {
    fail(n, `unknown key ${key}`);
  }
}

/** Parses the `.colonizer/instructions.toml` subset — `[[rule]]` headers, `key = "string"`, single-line `key = ["a", "b"]`, `#` comments; anything else is a line-numbered error, so the caller can skip the file. */
export function parseInstructionsToml(text) {
  const rules = [];
  let rule = null;
  let ruleLine = 0;
  const closeRule = () => {
    if (rule && !rule.file) fail(ruleLine, `rule ${rules.length} has no file`);
  };
  const lines = String(text ?? '').split(/\r?\n/);
  for (let n = 0; n < lines.length; n++) {
    const line = stripComment(lines[n], n + 1).trim();
    if (!line) continue;
    const header = line.match(/^\[\[([A-Za-z0-9_-]+)\]\]$/);
    if (header) {
      if (header[1] !== 'rule') fail(n + 1, `unknown table [[${header[1]}]]`);
      closeRule();
      rule = { file: null, paths: [], labels: [] };
      ruleLine = n + 1;
      rules.push(rule);
      continue;
    }
    const kv = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.+)$/);
    if (!kv || !rule) fail(n + 1, 'expected a [[rule]] header or key = "value"');
    const value = kv[2].trim();
    if (value.startsWith('"')) assign(rule, kv[1], parseString(value, n + 1), n + 1);
    else if (value.startsWith('[')) assign(rule, kv[1], parseArray(value, n + 1), n + 1);
    else fail(n + 1, `unsupported value for ${kv[1]}: expected "string" or ["a", "b"]`);
  }
  closeRule();
  return { rules };
}

/** Reads `.colonizer/instructions.toml`: absent means no rules, unparseable means no rules and one warning — a misconfigured file never breaks the session. */
export function loadInstructionsConfig(workspace, readFile = (path) => readFileSync(path, 'utf8')) {
  let text;
  try {
    text = readFile(join(workspace, INSTRUCTIONS_TOML));
  } catch {
    return { rules: [], warning: null };
  }
  try {
    return { rules: parseInstructionsToml(text).rules, warning: null };
  } catch (err) {
    return { rules: [], warning: `${INSTRUCTIONS_TOML} ignored: ${err?.message ?? err}` };
  }
}

/** Comma-separated `COLONIZER_TASK_LABELS` → label list. */
export function parseLabels(env) {
  return String(env ?? '').split(',').map((label) => label.trim()).filter(Boolean);
}

function capFragment(buf) {
  if (buf.length <= MAX_FRAGMENT_BYTES) return buf.toString('utf8');
  // Node renders a character the cut split in half as one trailing U+FFFD; drop it instead.
  const head = buf.subarray(0, MAX_FRAGMENT_BYTES).toString('utf8').replace(/�$/, '');
  return `${head}\n[… truncated ${buf.length - MAX_FRAGMENT_BYTES} bytes …]`;
}

const xmlAttr = (value) => String(value).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

const fragmentBlock = (frag) =>
  `<conditional-instructions file="${xmlAttr(frag.file)}" reason="${xmlAttr(frag.reason)}">\n${frag.content.trimEnd()}\n</conditional-instructions>`;

/** Per-window state and the hook handlers the runner registers. Windows are keyed by the hook input's `agent_id` (`main` for the orchestrator): a subagent's context is its own. */
export class ConditionalInstructions {
  #workspace;
  #labels;
  #log;
  #windows = new Map();
  #fragmentCache = new Map();
  #rules = [];
  #rulesLoaded = false;
  #warned = new Set();

  /**
   * @param {object} [args]
   * @param {string} [args.workspace]  the repo the colony works in
   * @param {string[]} [args.labels]   the task's labels, for `.colonizer/instructions.toml` rules
   * @param {(event: { level: string, message: string }) => void} [args.log]  colony log
   */
  constructor({ workspace = process.cwd(), labels = [], log = () => {} } = {}) {
    this.#workspace = resolve(workspace);
    this.#labels = Array.isArray(labels) ? labels : [];
    this.#log = typeof log === 'function' ? log : () => {};
  }

  /** PreToolUse: record the call's paths, inject what now holds. Never a permission decision. */
  preToolUse(input) {
    return this.#guarded('PreToolUse', () =>
      this.#evaluate(input, pathsFromToolInput(input?.tool_name, input?.tool_input, { workspace: this.#workspace })));
  }

  /** UserPromptSubmit: same, from the paths the prompt itself names. */
  userPromptSubmit(input) {
    return this.#guarded('UserPromptSubmit', () => {
      const win = this.#windowOf(input);
      win.turn += 1;
      return this.#evaluate(input, tokensFromText(input?.prompt, { workspace: this.#workspace }));
    });
  }

  /** SessionStart: after a rebuild (a compaction, or startup/resume) re-inject what still holds. */
  sessionStart(input) {
    return this.#guarded('SessionStart', () => {
      const win = this.#windowOf(input);
      win.injected.clear();
      win.pendingCompact = false;
      win.reloading = input?.source === 'compact';
      const result = this.#evaluate(input, []);
      win.freshRebuild = true; // after #evaluate, which clears it: the rebuild is the last thing that happened
      return result;
    });
  }

  /** The runner's fallback for a compaction it saw as a compact_boundary but no SessionStart announced: the next hook call re-injects what holds. Main window only. */
  markCompacted() {
    try {
      const win = this.#windows.get('main');
      if (win && !win.freshRebuild) win.pendingCompact = true; // a SessionStart rebuild just before was for this very compaction
    } catch {
      // never break the caller that reports a compaction
    }
  }

  #agentKey(input) {
    return typeof input?.agent_id === 'string' && input.agent_id ? input.agent_id : 'main';
  }

  #windowOf(input) {
    const key = this.#agentKey(input);
    let win = this.#windows.get(key);
    if (!win) {
      win = { injected: new Set(), recent: [], turn: 0, pendingCompact: false, freshRebuild: false, reloading: false };
      this.#windows.set(key, win);
    }
    return win;
  }

  #touch(win, rel) {
    const known = win.recent.indexOf(rel);
    if (known !== -1) win.recent.splice(known, 1);
    win.recent.unshift(rel);
    if (win.recent.length > MAX_RECENT_PATHS) win.recent.length = MAX_RECENT_PATHS;
  }

  #evaluate(input, rawPaths) {
    const win = this.#windowOf(input);
    for (const raw of rawPaths) {
      const rel = relInside(this.#workspace, raw);
      if (rel) this.#touch(win, rel);
    }
    // The fallback path: a compaction no SessionStart announced. Forget pre-compaction injections —
    // the summarised context no longer has them — and let the conditions below decide what is back.
    win.freshRebuild = false;
    let reloaded = win.reloading;
    if (win.pendingCompact) {
      win.pendingCompact = false;
      reloaded = true;
      win.injected.clear();
    }
    const agent = this.#agentKey(input);
    const loaded = [];
    for (const frag of this.#holding(win)) {
      if (win.injected.has(frag.file)) continue;
      win.injected.add(frag.file);
      loaded.push(frag);
      if (!reloaded) {
        this.#log({ level: 'info', message: `instructions: loaded ${frag.file} (matched ${frag.reason}) [agent ${agent}, turn ${win.turn}]` });
      }
    }
    if (reloaded) {
      win.reloading = false;
      if (loaded.length) {
        this.#log({ level: 'info', message: `instructions: reloaded after compaction: ${loaded.map((f) => f.file).join(', ')} [agent ${agent}, turn ${win.turn}]` });
      }
    }
    if (!loaded.length) return { continue: true };
    return { continue: true, hookSpecificOutput: { additionalContext: loaded.map(fragmentBlock).join('\n\n') } };
  }

  /** Every fragment whose condition holds right now, most recently touched first. */
  #holding(win) {
    const found = new Map();
    for (const rel of win.recent) {
      for (const frag of this.#dirFragments(rel)) {
        if (!found.has(frag.file)) found.set(frag.file, { ...frag, reason: rel });
      }
    }
    for (const rule of this.#config()) {
      const matchedLabels = rule.labels.filter((label) => this.#labels.includes(label));
      const reason = matchedLabels.length
        ? `label ${matchedLabels.join(', ')}`
        : win.recent.find((rel) => rule.paths.some((glob) => globMatches(glob, rel)));
      if (!reason) continue;
      const frag = this.#fragmentFile(rule.file);
      if (frag && !found.has(frag.file)) found.set(frag.file, { ...frag, reason });
    }
    return [...found.values()];
  }

  /** FOOTGUNS.md from the root down to the file's own directory, plus subdirectory AGENTS.md (the root one already loads as project instructions). */
  #dirFragments(rel) {
    const dirs = rel.split('/').slice(0, -1);
    const frags = [];
    for (let i = 0; i <= dirs.length; i++) {
      const prefix = dirs.slice(0, i).join('/');
      const footguns = this.#fragmentFile(`${prefix ? `${prefix}/` : ''}${FOOTGUNS_BASENAME}`);
      if (footguns) frags.push(footguns);
      if (prefix) {
        const agents = this.#fragmentFile(`${prefix}/${AGENTS_BASENAME}`);
        if (agents) frags.push(agents);
      }
    }
    return frags;
  }

  /** One instruction file, resolved against the real workspace; anything ending up outside it — a `..` in the config, a symlink out — is refused with one warning. Cached per file. */
  #fragmentFile(relFile) {
    if (this.#fragmentCache.has(relFile)) return this.#fragmentCache.get(relFile);
    let frag = null;
    try {
      const base = realpathSync(this.#workspace);
      let real = null;
      try {
        real = realpathSync(resolve(base, relFile));
      } catch {
        // absent, or not a path
      }
      if (real) {
        const rel = relative(base, real);
        if (!rel || rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
          this.#warn(`${relFile} refused: it resolves outside the workspace`);
        } else {
          try {
            frag = { file: rel.split(sep).join('/'), content: capFragment(readFileSync(real)) };
          } catch {
            // a directory named FOOTGUNS.md, or an unreadable file: no fragment
          }
        }
      }
    } catch {
      // the workspace itself vanished; no fragments
    }
    this.#fragmentCache.set(relFile, frag);
    return frag;
  }

  #config() {
    if (!this.#rulesLoaded) {
      this.#rulesLoaded = true;
      const { rules, warning } = loadInstructionsConfig(this.#workspace);
      if (warning) this.#warn(warning);
      this.#rules = rules;
    }
    return this.#rules;
  }

  #warn(message) {
    if (this.#warned.has(message)) return;
    this.#warned.add(message);
    this.#log({ level: 'warn', message: `instructions: ${message}` });
  }

  #guarded(eventName, fn) {
    try {
      const result = fn();
      if (result.hookSpecificOutput) result.hookSpecificOutput.hookEventName = eventName;
      return result;
    } catch (err) {
      this.#log({ level: 'warn', message: `instructions: ${eventName} hook failed: ${err?.message ?? err}` });
      return { continue: true };
    }
  }
}
