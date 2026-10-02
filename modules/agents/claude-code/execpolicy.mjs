// Programmable exec policy (issue #471): rules that deny, ask or allow a Bash command and, for
// `bash x.sh` / `python x.py` / `node x.js`-style commands, the contents of the script it runs.
// This module is pure — every decision follows from the policy alone, so tests drive it without
// an SDK. The runner wires it up: a PreToolUse hook refuses (or asks), and canUseTool raises an
// `ask` as a colony question the operator (or the autonomy judge) answers. See README.md for the
// full picture; in short: a policy is `{ "rules": [...] }`, a rule matches when ALL its
// predicates hold (`command` regex, `touches` path globs, `script` regex over the script's
// contents, `writes_outside`), the first match in a layer wins, and across layers the STRICTEST
// decision wins (deny > ask > allow), so a later layer can only ever narrow. Layers: default
// (built in), install (`COLONIZER_EXEC_POLICY`), org (`COLONIZER_EXEC_POLICY_ORG`) and repo
// (`.colonizer/exec-policy.json`, read once at start); a malformed layer is dropped with a
// warning, so the policy fails closed on the default.

import { openSync, readSync, closeSync, statSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, join } from 'node:path';

export const EXEC_POLICY_DECISIONS = ['deny', 'ask', 'allow'];
export const EXEC_POLICY_REPO_FILE = join('.colonizer', 'exec-policy.json');
const EXEC_POLICY_MAX_BYTES = 64 * 1024; // a policy file is rules, not data; more is a mistake
const SCRIPT_MAX_BYTES = 256 * 1024;
const REGEX_MAX_CHARS = 500; // a rule regex is a pattern, not a program; longer is a mistake — the
// only cheap handle on ReDoS here, so a repo-layer rule does not get to be one either
const RANK = { deny: 3, ask: 2, allow: 1 };
const LOG_COMMAND_CHARS = 200;

// Interpreters (and `source` and `.`, which run a script in place) whose first non-flag argument
// names the script the command runs.
const INTERPRETERS = new Set(['bash', 'sh', 'zsh', 'dash', 'ksh', 'python', 'python3', 'node', 'deno', 'bun', 'ruby', 'perl', 'source', '.']);
const WRITE_WORDS = new Set(['cp', 'mv', 'rm', 'touch', 'mkdir', 'tee', 'truncate', 'install']);
const WRITE_TARGET_FLAGS = /^(-|--|[a-zA-Z]=)/;
// Write targets that are never "outside the repository": scratch space and the kernel's own sinks.
const NEVER_OUTSIDE = [/^\/tmp(\/|$)/, /^\/var\/tmp(\/|$)/, /^\/dev\/(?:null|stdout|stderr|fd)\b/, /^\/run\/user\//];

// Call-shaped egress, not bare URLs: a script that calls out is what the rule is about. The direct
// `curl` command is deliberately NOT blocked by default — the colony's egress policy governs that.
const SCRIPT_EGRESS = [
  /\bcurl\s/, /\bwget\s/, /\bnc(?:at)?\s/, /\bsocat\s/, /\bssh\s/, /\bscp\s/, /\bsftp\s/,
  /\brequests\.(?:get|post|put|patch|delete|head|request)\s*\(/,
  /\burllib\.request\b/, /\burlopen\s*\(/, /\bhttp\.client\b/, /\bsocket\.socket\b/,
  /\bfetch\s*\(/, /\baxios[.(]/, /\bhttps?\.(?:get|post|request)\s*\(/, /\bXMLHttpRequest\b/,
];

// The first layer, present for every colony. It mirrors the path policy's DEFAULT_MASKED
// (crates/colonizer/src/path_policy.rs): ~/.ssh and the credential files the path policy masks are
// denied in the command and in any script it runs; writing outside the repository asks.
export function defaultPolicy() {
  return {
    rules: [
      {
        id: 'secret-paths',
        decision: 'deny',
        reason: 'the command reaches a credential path the colony must not read',
        touches: ['~/.ssh', '.env', '.env.*', '.envrc', '.npmrc', '.netrc', '.git-credentials', '.pypirc',
          '!*.example', '!*.sample', '!*.template', '!*.dist'],
      },
      {
        id: 'script-egress',
        decision: 'deny',
        reason: 'the script this command runs talks to the network',
        script: SCRIPT_EGRESS,
      },
      {
        id: 'writes-outside-repo',
        decision: 'ask',
        reason: 'the command writes outside the repository',
        writes_outside: true,
      },
    ],
  };
}

/** Compiles a predicate value (string or RegExp, or an array of them) to regexes, or null. */
function compileRegexes(value) {
  const list = Array.isArray(value) ? value : [value];
  if (!list.length || list.some((p) => typeof p !== 'string' && !(p instanceof RegExp))) return null;
  if (list.some((p) => typeof p === 'string' && p.length > REGEX_MAX_CHARS)) return null;
  if (list.every((p) => p === '')) return null;
  try {
    return list.map((p) => new RegExp(p));
  } catch {
    return null;
  }
}

/** Compiles one policy layer's JSON text or object to rules, or null when it is malformed. */
export function parsePolicy(input) {
  let parsed = input;
  if (typeof input === 'string') {
    if (input.length > EXEC_POLICY_MAX_BYTES) return null;
    try {
      parsed = JSON.parse(input);
    } catch {
      return null;
    }
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed) || !Array.isArray(parsed.rules)) return null;
  const rules = [];
  for (const [i, raw] of (parsed.rules).entries()) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) continue;
    const decision = String(raw.decision ?? '').toLowerCase();
    if (!EXEC_POLICY_DECISIONS.includes(decision)) continue;
    const rule = {
      id: typeof raw.id === 'string' && raw.id.trim() ? raw.id.trim() : `rule-${i + 1}`,
      decision,
      reason: typeof raw.reason === 'string' && raw.reason.trim() ? raw.reason : 'the colony’s exec policy refuses this',
      predicates: [],
    };
    const command = compileRegexes(raw.command);
    if (command) rule.predicates.push({ kind: 'command', res: command });
    const script = compileRegexes(raw.script);
    if (script) rule.predicates.push({ kind: 'script', res: script });
    if (Array.isArray(raw.touches)) {
      // An entry starting with `!` excludes: a token counts only when an include glob matches it
      // and no exclusion does, so `['.env.*', '!*.example']` denies `.env.local` but not the
      // committed template `.env.example`.
      const touches = raw.touches.filter((p) => typeof p === 'string' && p.trim() && p[0] !== '!');
      const keepOut = raw.touches
        .filter((p) => typeof p === 'string' && p.startsWith('!') && p.trim().length > 1)
        .map((p) => pathGlobRe(p.slice(1)));
      if (touches.length) rule.predicates.push({ kind: 'touches', globs: touches, res: touches.map(pathGlobRe), keepOut });
    }
    if (raw.writes_outside === true) rule.predicates.push({ kind: 'writes_outside' });
    // A rule with none of the four known predicates would match every command by accident, so it
    // is dropped; a deliberate catch-all is `"command": "."` (or `""`).
    if (rule.predicates.length) rules.push(rule);
  }
  return { rules };
}

/**
 * The colony's policy, layered: default → install → org → repo. Called once at runner start; the
 * repo file read here is the one the whole run keeps, so the agent rewriting it mid-run cannot
 * widen anything (and the layering would not let it anyway). A layer that does not parse is
 * dropped with a warning, never fatal: the default keeps enforcing.
 */
export function loadExecPolicy(env = process.env, { cwd = process.cwd(), readFile = readTextCapped } = {}) {
  const layers = [{ name: 'default', rules: parsePolicy({ rules: defaultPolicy().rules }).rules }];
  const warnings = [];
  const add = (name, raw) => {
    const policy = parsePolicy(raw);
    if (!policy) {
      warnings.push(`ignoring the ${name} exec policy: expected {"rules": [{"id", "decision", "reason", "command"|"script"|"touches"|"writes_outside"}]}`);
      return;
    }
    layers.push({ name, rules: policy.rules });
  };
  if (env.COLONIZER_EXEC_POLICY) add('install', env.COLONIZER_EXEC_POLICY);
  if (env.COLONIZER_EXEC_POLICY_ORG) add('org', env.COLONIZER_EXEC_POLICY_ORG);
  const repo = readFile(join(cwd, EXEC_POLICY_REPO_FILE), EXEC_POLICY_MAX_BYTES);
  if (repo) add('repo', repo);
  return { layers, warnings };
}

/**
 * The decision for one Bash command, or null when no rule matches. Within a layer the first
 * matching rule wins; across layers the strictest decision wins, and on a tie the earlier layer's
 * rule is the one reported (the default names itself first, which is the one an operator reads).
 * @param {{ layers: {name: string, rules: object[]}[] }} policy   as loadExecPolicy returned
 * @param {string} command                                         the Bash tool's command
 * @param {object} [opts]   { cwd, readFile } — cwd is the repository root the command runs in
 * @returns {{ decision: string, rule: string, layer: string, reason: string } | null}
 */
export function evaluateExecPolicy(policy, command, opts = {}) {
  if (!policy || typeof command !== 'string' || !command) return null;
  const ctx = buildContext(command, opts);
  let best = null;
  for (const layer of policy.layers) {
    for (const rule of layer.rules) {
      if (!ruleMatches(rule, ctx)) continue;
      const hit = { decision: rule.decision, rule: rule.id, layer: layer.name, reason: rule.reason };
      if (!best || RANK[hit.decision] > RANK[best.decision]) best = hit;
      break; // first matching rule in this layer wins
    }
  }
  return best;
}

/** The reason a decision carries, naming the rule and its layer. */
export function execPolicyReason(hit) {
  return `exec policy rule \`${hit.rule}\` (${hit.layer}): ${hit.reason}`;
}

/** A command on one line, capped, for the log line and the question text. */
const oneLine = (command, max) => {
  const flat = String(command).replace(/\s+/g, ' ').trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
};

/** One harness-log line per decision (agentd turns runner stderr lines into `log` events). */
export function execPolicyLogLine(hit, command) {
  return `exec policy: ${hit.decision} rule=${hit.rule} layer=${hit.layer} command=${oneLine(command, LOG_COMMAND_CHARS)}`;
}

/**
 * The `kind` a question event carries when an exec-policy `ask` raised it (issue #759): the Bash
 * call that asked is still in flight inside a live agent, so the mothership must not suspend the
 * colony while it waits — the call would die with the microVM and the agent that made it.
 */
export const EXEC_POLICY_QUESTION_KIND = 'exec_policy';

/** The colony question an `ask` decision becomes: the rule in the text, Allow and Deny options. */
export function execPolicyQuestion(hit, command) {
  const truncated = oneLine(command, 400);
  return {
    questions: [
      {
        question: `Exec policy rule \`${hit.rule}\` (${hit.layer}) asks before running: ${truncated}. ${hit.reason}. Run it?`,
        header: 'Exec policy',
        multiSelect: false,
        options: [
          { label: 'Allow', description: 'Run this command; the colony will not ask again for the same command' },
          { label: 'Deny', description: 'Refuse the command; the agent sees the policy reason' },
        ],
      },
    ],
  };
}

/**
 * The colony's memory of the commands its operator allowed (issue #759). An Allow is kept per
 * (rule, layer, command with its whitespace collapsed), so the agent that retries the command —
 * or a subagent the lead spawns in place of one that died — is not asked again for it, while a
 * different command, or the same one under a different rule, still asks. Only an `ask` is ever
 * looked up here: a deny is refused before the cache is consulted, so nothing in it can soften a
 * deny. It lives in the runner process — one per colony run — and never on disk, where the agent
 * could write itself an approval; a colony restored in a fresh microVM therefore asks afresh.
 */
export function createExecAllowCache() {
  const allowed = new Set();
  const key = (hit, command) => `${hit.rule}\u0000${hit.layer}\u0000${String(command).replace(/\s+/g, ' ').trim()}`;
  return {
    /** True when this `ask` was already allowed for this command. */
    has: (hit, command) => hit?.decision === 'ask' && allowed.has(key(hit, command)),
    /** Records an operator's Allow of this `ask`; anything but an `ask` is ignored. */
    remember: (hit, command) => {
      if (hit?.decision === 'ask') allowed.add(key(hit, command));
    },
    get size() {
      return allowed.size;
    },
  };
}

// --- matching ------------------------------------------------------------------

/** One rule: every predicate present must hold. */
function ruleMatches(rule, ctx) {
  for (const p of rule.predicates) {
    switch (p.kind) {
      case 'command':
        if (!p.res.some((re) => re.test(ctx.command))) return false;
        break;
      case 'script':
        // No readable script, no match: the rule is about what the command runs, and a command
        // that runs nothing readable cannot show it (the default layer's other rules still hold).
        if (!ctx.scripts.some((s) => p.res.some((re) => re.test(s.text)))) return false;
        break;
      case 'touches':
        if (!ctx.tokens.some((t) => p.res.some((re) => re.test(t)) && !p.keepOut.some((re) => re.test(t)))) return false;
        break;
      case 'writes_outside':
        if (!ctx.writesOutside) return false;
        break;
    }
  }
  return true;
}

// --- commands ------------------------------------------------------------------

/** A `touches` glob to a regex: components at any depth, `*` never crossing a `/`, `~` is $HOME. */
function pathGlobRe(glob) {
  const body = escapeRegex(expandTilde(glob)).replace(/\\\*/g, '[^/]*').replace(/\\\?/g, '[^/]');
  return new RegExp(`(?:^|/)${body}(?:/|$)`);
}

function escapeRegex(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** `~`, `~/…`, `$HOME` and `${HOME}` to the real home; anything else unchanged. */
function expandTilde(path) {
  const text = String(path);
  if (text === '~') return homedir();
  if (text.startsWith('~/')) return join(homedir(), text.slice(2));
  return text.replace(/^(?:\$\{HOME\}|\$HOME)(?=\/|$)/, homedir()); // the same place, spelled out
}

/** Splits a compound command on `&&`, `||`, `;`, `|`, `&` and newlines, quotes respected. */
export function splitCommands(command) {
  const segments = [];
  let current = '';
  let quote = null;
  for (let i = 0; i < command.length; i++) {
    const ch = command[i];
    if (quote) {
      current += ch;
      if (ch === quote) quote = null;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      current += ch;
    } else if (ch === ';' || ch === '\n' || ch === '|' || ch === '&') {
      if ((ch === '|' || ch === '&') && command[i + 1] === ch) i++;
      segments.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  segments.push(current);
  return segments.map((s) => s.trim()).filter(Boolean);
}

/** The words of one command segment, cleaned of the punctuation that glues paths to words. */
function words(text) {
  return String(text ?? '')
    .replace(/\$\{HOME\}/g, '$HOME') // before the split: the braces would break the word apart
    .split(/[\s;|&<>()"'`\[\]{}]+/)
    .map((w) => w.replace(/^[@:=+]+/, '').replace(/[,;:]+$/, ''))
    .filter((w) => w.length > 1);
}

/** A segment's words past its env assignments and a leading sudo/env/command/exec. */
function commandWords(segment) {
  const ws = words(segment);
  let i = 0;
  while (i < ws.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(ws[i])) i++; // env assignments
  if (['sudo', 'env', 'command', 'exec'].includes(ws[i])) i++;
  return ws.slice(i);
}

/** What a segment names as the script it runs: `bash x.sh`, `python -O x.py`, `./x.sh`, `/opt/x.rb`. */
function scriptArg(segment) {
  // `. x.sh` sources in place, like `bash x.sh`; the lone dot never survives words().
  const ws = /^\s*\.\s+\S/.test(segment) ? ['.', ...commandWords(segment)] : commandWords(segment);
  const head = ws[0] ?? '';
  if (INTERPRETERS.has(basename(head))) {
    for (const w of ws.slice(1)) {
      if (!w.startsWith('-')) return expandTilde(w); // the first non-flag argument is the script
    }
    return null;
  }
  const direct = /^(\.{1,2}\/)?[\w@.-]+\.(sh|bash|py|js|mjs|cjs|rb|pl)$/.test(head) && head.includes('/');
  return direct ? expandTilde(head) : null;
}

/** Reads a file (a script the command runs) up to SCRIPT_MAX_BYTES, or null when unreadable. */
function readScriptFile(path) {
  try {
    const stats = statSync(path);
    if (!stats.isFile()) return null;
    const wanted = Math.min(stats.size, SCRIPT_MAX_BYTES);
    const fd = openSync(path, 'r');
    try {
      const buffer = Buffer.alloc(wanted);
      const got = readSync(fd, buffer, 0, wanted, 0);
      return buffer.toString('utf8', 0, got);
    } finally {
      closeSync(fd);
    }
  } catch {
    return null;
  }
}

/** Reads the repo layer's policy file, capped; null when absent or unreadable. */
function readTextCapped(path, maxBytes) {
  try {
    const stats = statSync(path);
    if (!stats.isFile() || stats.size > maxBytes) return null;
    return readFileSync(path, 'utf8');
  } catch {
    return null;
  }
}

/** The write targets one segment names: redirects, tee, cp/mv/rm/touch/mkdir/truncate, dd of=. */
function writeTargets(segment) {
  const targets = [];
  // Redirect targets, spaced (`cmd > /x`) and glued (`cmd>/x`); a `/` right before the `>` is a
  // character in a quoted pattern (`sed 's/</>/g'`), not a redirect.
  const redirects = segment.matchAll(/(?:^|[\s;|&])(?:\d*>?>|&>?>?)\s*([^\s;|&]+)|[^\s;|<>/](>>?>?)\s*(\/[^\s;|&]+)/g);
  for (const [, target, , glued] of redirects) targets.push(target ?? glued); // the second form is a glued `x>/abs/path`
  const ws = commandWords(segment);
  if (ws[0] === 'dd') {
    for (const w of ws.slice(1)) {
      const of = w.match(/^of=(.+)$/);
      if (of) targets.push(of[1]);
    }
  } else if (WRITE_WORDS.has(ws[0])) {
    for (const w of ws.slice(1)) {
      if (!WRITE_TARGET_FLAGS.test(w)) targets.push(w);
    }
  }
  return targets;
}

/** True when a segment writes an absolute path outside cwd (the repo root), /tmp and /dev aside. */
function writesOutsideRepo(segment, cwd) {
  for (const raw of writeTargets(segment)) {
    const target = expandTilde(raw);
    if (!target.startsWith('/')) continue; // relative: inside the worktree
    if (NEVER_OUTSIDE.some((re) => re.test(target))) continue;
    if (target === cwd || target.startsWith(`${cwd}/`)) continue;
    return true;
  }
  return false;
}

/**
 * Everything the predicates see, derived from the command alone: its segments, the tokens the
 * `touches` globs match against (the command's words and every script's words), the scripts it
 * runs (resolved against the repo root, contents capped), and whether anything is written outside.
 */
function buildContext(command, { cwd = process.cwd(), readFile = readScriptFile } = {}) {
  const segments = splitCommands(command);
  const scripts = [];
  const seen = new Set();
  for (const segment of segments) {
    const arg = scriptArg(segment);
    if (!arg) continue;
    const path = arg.startsWith('/') ? arg : join(cwd, arg);
    if (seen.has(path)) continue;
    seen.add(path);
    const text = readFile(path);
    if (text) scripts.push({ path, text });
  }
  const tokens = new Set();
  for (const word of [...segments.flatMap((s) => words(s)), ...scripts.flatMap((s) => words(s.text))]) {
    tokens.add(expandTilde(word));
  }
  return {
    command,
    segments,
    tokens: [...tokens],
    scripts,
    writesOutside: segments.some((segment) => writesOutsideRepo(segment, cwd)),
  };
}
