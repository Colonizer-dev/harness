// Subagent effort (COLONIZER_SUBAGENT_EFFORT). Claude Code gives a subagent the effort its definition
// names and otherwise the session's, and has no setting for a subagent default: the built-in agents
// name none, so without these every subagent reasons at the orchestrator's effort. Setting the effort
// means redefining the built-ins the orchestrator actually delegates to, under their own names, so a
// Task call keeps landing where it did. An `agents` entry replaces the built-in of the same name.
//
// Every role is an ant defined in an agent file, `crew/agents/<file>.md` (issue #1163): YAML
// frontmatter carrying the Claude Code fields (`name`, `description`, `tools`, `disallowedTools`,
// `model`, `effort`) plus the Colonizer identity (`skillsets` and the `ant:` block), then the body,
// which is the agent's system prompt. Nothing consumes the identity yet; the body is what a subagent
// is run with. The pack is a plugin (`crew/plugin.json`) so a colony can ship it like any other.
//
// The `general-purpose` and `Explore` descriptions and prompts are verbatim copies of the built-ins
// in the build crates/colonizer/claude-code.lock pins, snapshotted in vendor/claude-code-builtins.json;
// the Explore prompt is the variant a colony gets (a POSIX guest searching with find and grep via
// Bash). A Claude Code bump that rewrites a built-in fails CI until they are refreshed: `sh
// scripts/fetch-agent-binary.sh && node scripts/builtin-subagents.mjs --write dist/bin/claude-guest`
// re-extracts the snapshot and shows the diff. The two are redefined only for the effort (see
// BUILTIN_REDEFINITIONS); the other crew ants always ship. `model` is omitted unless a file sets one:
// CLAUDE_CODE_SUBAGENT_MODEL (COLONIZER_SUBAGENT_MODEL) then applies, exactly as it did for the
// built-ins.

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** The built-in crew pack that ships with this module. In a checkout this is the module dir; inside a
 * colony VM the whole module dir is mounted read-only at /opt/colonizer/agent, so the same relative
 * resolution works. */
const CREW_DIR = new URL('./crew/agents/', import.meta.url);

/** The castes an ant can belong to; `queen` is the orchestrator's own, never an SDK subagent. */
const CASTES = new Set(['forager', 'soldier', 'weaver', 'honeypot', 'scout', 'worker', 'queen']);

/** The keys an `ant:` block may carry. Unlike the top level — where Claude Code adds fields of its
 * own over time and unknown ones are allowed — the ant block is Colonizer's, so a typo'd key is an
 * error rather than a silently ignored one. */
const ANT_KEYS = new Set(['display_name', 'caste', 'title', 'colors', 'move']);

/** The efforts an agent file may name for itself. */
const FILE_EFFORTS = new Set(['low', 'medium', 'high']);

const HEX_COLOR = /^#[0-9a-f]{6}$/i;

/** A plain directory name, mirroring `is_plain_name` in crates/colonizer/src/util.rs. */
const PLAIN_NAME = /^(?!\.)(?!.*\.\.)[^/\\:,\0]+$/;

/** The crew ants that redefine a Claude Code built-in under its own name. They are emitted only when
 * an effort is set: that effort is the only thing being changed, and unset the built-ins stay in
 * place and inherit the orchestrator's effort. */
const BUILTIN_REDEFINITIONS = new Set(['general-purpose', 'Explore']);

/** Split on `sep` at the top level only: commas inside double quotes or braces do not split. */
const splitTop = (text, sep) => {
  const parts = [];
  let current = '';
  let quoted = false;
  let braces = 0;
  for (const char of text) {
    if (char === '"') quoted = !quoted;
    else if (!quoted && char === '{') braces++;
    else if (!quoted && char === '}') braces--;
    if (char === sep && !quoted && braces === 0) {
      parts.push(current);
      current = '';
      continue;
    }
    current += char;
  }
  parts.push(current);
  return parts;
};

/** A scalar, bare (trimmed) or double-quoted with the quotes stripped. */
const scalar = (raw) => {
  const text = raw.trim();
  if (!text.startsWith('"')) return text;
  if (!text.endsWith('"') || text.length < 2) throw new Error(`unterminated quoted scalar ${raw.trim()}`);
  return text.slice(1, -1);
};

/** An inline array `[a, b, "c"]`: non-empty string items. */
const inlineArray = (raw) => {
  const text = raw.trim();
  if (!text.startsWith('[') || !text.endsWith(']')) throw new Error(`expected an inline array, got ${text}`);
  const inner = text.slice(1, -1).trim();
  if (!inner) return [];
  return splitTop(inner, ',').map((item) => {
    const value = scalar(item);
    if (!value) throw new Error(`inline array ${text} has an empty item`);
    return value;
  });
};

/** An inline map `{ k: "v", k2: "v2" }`. */
const inlineMap = (raw) => {
  const text = raw.trim();
  if (!text.startsWith('{') || !text.endsWith('}')) throw new Error(`expected an inline map, got ${text}`);
  const inner = text.slice(1, -1).trim();
  const map = {};
  if (!inner) return map;
  for (const item of splitTop(inner, ',')) {
    const match = /^([^:]+):(.*)$/.exec(item.trim());
    if (!match) throw new Error(`inline map ${text} has an item that is not \`key: value\`: ${item.trim()}`);
    map[match[1].trim()] = scalar(match[2]);
  }
  return map;
};

/** A frontmatter value: an inline array, an inline map or a scalar, by its first character. */
const value = (raw) => {
  const text = raw.trim();
  if (text.startsWith('[')) return inlineArray(text);
  if (text.startsWith('{')) return inlineMap(text);
  return scalar(text);
};

const nonEmptyString = (value, what) => {
  if (typeof value !== 'string' || !value.trim()) throw new Error(`\`${what}\` must be a non-empty string`);
};

/**
 * Parses one agent file: a `---` frontmatter block, then the body, which is the agent's system
 * prompt. Supports the YAML subset the crew pack sticks to — `key: value` scalars (bare or
 * double-quoted), inline arrays, inline maps, and one nested block level via exactly-two-space
 * indentation (`ant:`). Mirrors the parser the mothership is gaining for user packs; keep the two
 * regular. Unknown keys are ignored, so a pack survives Claude Code growing a field.
 * @param {string} text the whole file
 * @param {string} file a name for it, used in every error
 * @returns {{ file, name, description, body, tools?, disallowedTools?, model?, effort?, skillsets?, ant? }}
 */
export function parseAgentMd(text, file) {
  try {
    return parseAgentMdOrThrow(text, file);
  } catch (err) {
    throw new Error(`${file}: ${err.message}`);
  }
}

function parseAgentMdOrThrow(text, file) {
  const lines = text.split('\n');
  if ((lines[0] ?? '').trim() !== '---') throw new Error('does not open with a `---` frontmatter line');
  const close = lines.findIndex((line, i) => i > 0 && line.trim() === '---');
  if (close < 0) throw new Error('the frontmatter is never closed with a `---` line');
  // The body is the system prompt: everything after the closing line, minus the file's final newline
  // (a text file ends with one; the prompt itself must not).
  const body = lines.slice(close + 1).join('\n').replace(/\n$/, '');
  if (!body.trim()) throw new Error('has no body, and the body is the agent\'s system prompt');

  const top = {};
  let ant = null;
  let inAnt = false;
  for (const line of lines.slice(1, close)) {
    if (!line.trim()) continue;
    if (line.startsWith(' ') || line.startsWith('\t')) {
      if (!inAnt) throw new Error(`indented line outside a nested block (only \`ant:\` nests, two spaces): ${JSON.stringify(line)}`);
      if (!line.startsWith('  ') || line.startsWith('   ')) throw new Error(`a nested line indents by exactly two spaces: ${JSON.stringify(line)}`);
      const match = /^([A-Za-z0-9_-]+)\s*:(.*)$/.exec(line.trimStart());
      if (!match) throw new Error(`not a \`key: value\` line: ${JSON.stringify(line)}`);
      if (!ANT_KEYS.has(match[1])) throw new Error(`unknown \`ant:\` key ${JSON.stringify(match[1])} (one of ${[...ANT_KEYS].join(', ')})`);
      ant[match[1]] = value(match[2]);
      continue;
    }
    inAnt = false;
    const match = /^([A-Za-z0-9_-]+)\s*:(.*)$/.exec(line);
    if (!match) throw new Error(`not a \`key: value\` line: ${JSON.stringify(line)}`);
    if (match[1] === 'ant') {
      if (match[2].trim()) throw new Error('`ant:` is a nested block (two-space indented keys), not a scalar');
      ant = {};
      inAnt = true;
      continue;
    }
    top[match[1]] = value(match[2]);
  }

  if (top.name === undefined) throw new Error('has no `name:` in the frontmatter');
  if (typeof top.name !== 'string' || !PLAIN_NAME.test(top.name)) {
    throw new Error(`\`name: ${top.name}\` is not a plain directory name`);
  }
  if (top.description === undefined) throw new Error('has no `description:` in the frontmatter');
  nonEmptyString(top.description, 'description');
  if (top.model !== undefined) nonEmptyString(top.model, 'model');
  if (top.effort !== undefined && !FILE_EFFORTS.has(top.effort)) {
    throw new Error(`\`effort: ${top.effort}\` is not one of ${[...FILE_EFFORTS].join(', ')}`);
  }
  for (const key of ['tools', 'disallowedTools', 'skillsets']) {
    if (top[key] === undefined) continue;
    if (!Array.isArray(top[key])) throw new Error(`\`${key}\` must be an inline array`);
    for (const item of top[key]) {
      if (typeof item !== 'string' || !item) throw new Error(`\`${key}\` has a non-string or empty item`);
    }
  }
  for (const item of top.skillsets ?? []) {
    if (!PLAIN_NAME.test(item)) throw new Error(`\`skillsets\` entry ${JSON.stringify(item)} is not a plain name`);
  }

  let antBlock;
  if (ant) {
    nonEmptyString(ant.display_name, 'ant.display_name');
    if (!CASTES.has(ant.caste)) throw new Error(`\`ant.caste: ${ant.caste}\` is not one of ${[...CASTES].join(', ')}`);
    if (ant.title !== undefined) nonEmptyString(ant.title, 'ant.title');
    if (ant.move !== undefined) nonEmptyString(ant.move, 'ant.move');
    if (ant.colors !== undefined) {
      const colors = ant.colors;
      if (typeof colors !== 'object' || Array.isArray(colors)) throw new Error('`ant.colors` must be an inline map');
      if (new Set(Object.keys(colors)).size !== 3 || Object.keys(colors).some((k) => !['body', 'dark', 'accent'].includes(k))) {
        throw new Error(`\`ant.colors\` must have exactly the keys body, dark and accent, got ${Object.keys(colors).join(', ')}`);
      }
      for (const key of ['body', 'dark', 'accent']) {
        if (!HEX_COLOR.test(colors[key])) throw new Error(`\`ant.colors.${key}: ${colors[key]}\` is not a "#rrggbb" hex color`);
      }
    }
    antBlock = ant;
  }

  return {
    file,
    name: top.name,
    description: top.description,
    body,
    ...(top.tools !== undefined && { tools: top.tools }),
    ...(top.disallowedTools !== undefined && { disallowedTools: top.disallowedTools }),
    ...(top.model !== undefined && { model: top.model }),
    ...(top.effort !== undefined && { effort: top.effort }),
    ...(top.skillsets !== undefined && { skillsets: top.skillsets }),
    ...(antBlock && { ant: antBlock }),
  };
}

let crewCache;

/**
 * The built-in crew pack's agent files, parsed, sorted by filename (the order `subagentDefinitions`
 * emits them in). Cached at first load: the files ship with the module and do not change mid-run.
 * @param {string|URL} [dir] overrides the pack directory, for tests
 */
export function loadCrew(dir = CREW_DIR) {
  const path = typeof dir === 'string' ? dir : fileURLToPath(dir);
  if (path === fileURLToPath(CREW_DIR) && crewCache) return crewCache;
  let files;
  try {
    files = readdirSync(path).filter((name) => name.endsWith('.md')).sort();
  } catch (err) {
    throw new Error(`${path}: cannot read the crew's agents directory: ${err.message}`);
  }
  if (!files.length) throw new Error(`${path}: no agent files in the crew pack`);
  const crew = files.map((name) => parseAgentMd(readFileSync(join(path, name), 'utf8'), name));
  // Claude Code addresses an agent by name, so two files answering to one name are ambiguous by
  // construction — the same rule `validate_ants` in crates/colonizer/src/plugins.rs applies.
  const seen = new Map();
  for (const agent of crew) {
    const first = seen.get(agent.name.toLowerCase());
    if (first) throw new Error(`${path}: agent ${JSON.stringify(agent.name)} is defined by both ${first} and ${agent.file}: agent names must be unique within a pack`);
    seen.set(agent.name.toLowerCase(), agent.file);
  }
  if (path === fileURLToPath(CREW_DIR)) crewCache = crew;
  return crew;
}

/** Explore's deny list, as the crew's scout.md carries it: the prompt forbids writing, this enforces
 * it. At least the built-in's own deny list (the Artifact tools publish pages), plus Task and Agent,
 * its older name; CI checks it against the snapshot. `repo-explorer` reuses it — it is as read-only
 * as Explore. */
export const EXPLORE_DISALLOWED = (() => {
  const scout = loadCrew().find((agent) => agent.name === 'Explore');
  if (!scout) throw new Error('the crew pack has no `name: Explore` agent (crew/agents/scout.md)');
  if (!scout.disallowedTools) throw new Error('crew/agents/scout.md: has no `disallowedTools:`, but Explore must stay read-only');
  return scout.disallowedTools;
})();

const REPO_EXPLORER_PROMPT = [
  "You are a repository-exploration specialist for Claude Code, Anthropic's official CLI for Claude. Like Explore, you are READ-ONLY: you locate and explain code, you never modify it.",
  '',
  '=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===',
  'You are STRICTLY PROHIBITED from creating, editing, deleting, moving or copying files, from writing anywhere including /tmp, and from running any command that changes system state.',
  '',
  'Before you reach for find or grep, call the Skill tool and check whether this colony ships a retrieval skill suited to the question. For example, a skill named `graft` answers "how does X work", "where is Y defined", "who calls Z" and "what would changing this break" from a code map, with exact file:line -- faster and more precise than text search. Prefer a fitting skill\'s own instructions over raw search.',
  '',
  'Fall back to find/grep via Bash, exactly as the Explore agent would, when no shipped skill fits the question, or the one you tried is unavailable in this colony or comes back empty.',
  '',
  'Guidelines:',
  '- Try the fitting skill first; do not grep for something a skill already answers directly.',
  '- Use Bash ONLY for read-only operations (ls, git status, git log, git diff, find, grep, cat, head, tail).',
  '- Use Read when you know the specific file path you need.',
  "- Adapt your search approach based on the thoroughness level the caller specifies.",
  '- Communicate your final report directly as a regular message -- do NOT attempt to create files.',
  '',
  "Complete the caller's exploration request efficiently and report your findings clearly, with file:line references.",
].join('\n');

/**
 * The colony's subagents: the crew pack's ants (every crew file but the queen — she is the main
 * thread, never an SDK subagent), plus a first-party read-only `repo-explorer` that stays in code.
 * Keyed by agent type, the shape the SDK's `agents` option takes.
 * @param {string} [effort] one of EFFORT_LEVELS; the caller validates it. Omit it to leave the two
 *   redefined built-ins out (they keep inheriting the orchestrator's effort) and ship only the
 *   always-on crew ants and `repo-explorer`.
 */
export function subagentDefinitions(effort) {
  const withEffort = (def) => (effort ? { ...def, effort } : def);
  const definitions = {};
  for (const agent of loadCrew()) {
    if (agent.ant?.caste === 'queen') continue;
    if (BUILTIN_REDEFINITIONS.has(agent.name) && !effort) continue;
    definitions[agent.name] = withEffort({
      description: agent.description,
      prompt: agent.body,
      ...(agent.tools && { tools: agent.tools }),
      ...(agent.disallowedTools && { disallowedTools: agent.disallowedTools }),
      ...(agent.model && { model: agent.model }),
    });
  }
  definitions['repo-explorer'] = withEffort({
    description:
      'Read-only repository-exploration agent for structural questions -- "how does X work", "where is Y defined", "what calls Z", "what would this change break". Checks the Skill tool for a shipped retrieval skill (for example graft\'s code map) and prefers it over raw text search, falling back to find/grep like Explore when none applies. Use Explore instead for a plain literal or filename lookup that needs no code map.',
    prompt: REPO_EXPLORER_PROMPT,
    disallowedTools: EXPLORE_DISALLOWED,
  });
  return definitions;
}
