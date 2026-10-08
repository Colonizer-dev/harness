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

import { createHash } from 'node:crypto';
import { openSync, readSync, closeSync, statSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, join, normalize, posix } from 'node:path';

export const EXEC_POLICY_DECISIONS = ['deny', 'ask', 'allow'];
export const EXEC_POLICY_REPO_FILE = join('.colonizer', 'exec-policy.json');
/** Where the boot mounts the writable-bind list (boot.rs writes `vm_dir/host-mounts`, exposed at
 * guest `/colonizer`, alongside `path-policy`). */
export const HOST_MOUNTS_FILE = '/colonizer/host-mounts';
/** Where the boot writes the repository's own scripts as the base commit has them, `<object id>
 * <path>` per line (crates/colonizer/src/tracked_scripts.rs, issue #1239). */
export const TRACKED_SCRIPTS_FILE = '/colonizer/tracked-scripts';
const TRACKED_SCRIPTS_MAX_BYTES = 4 * 1024 * 1024;
const EXEC_POLICY_MAX_BYTES = 64 * 1024; // a policy file is rules, not data; more is a mistake
const HOST_MOUNTS_MAX_BYTES = 64 * 1024; // the list is paths, not data; more is a mistake
const SCRIPT_MAX_BYTES = 256 * 1024;
const REGEX_MAX_CHARS = 500; // a rule regex is a pattern, not a program; longer is a mistake — the
// only cheap handle on ReDoS here, so a repo-layer rule does not get to be one either
const RANK = { deny: 3, ask: 2, allow: 1 };
const LOG_COMMAND_CHARS = 200;

// Interpreters (and `source` and `.`, which run a script in place) whose first non-flag argument
// names the script the command runs.
const INTERPRETERS = new Set(['bash', 'sh', 'zsh', 'dash', 'ksh', 'python', 'python3', 'node', 'deno', 'bun', 'ruby', 'perl', 'source', '.']);
// Flags that make an interpreter parse its script and stop, running nothing (#1227): `bash -n`,
// `node --check`, `ruby -c`. Only when these are the interpreter's only flags; `bash -n -c …` or
// `bash -n -x …` is not recognised and is read as a run, as before.
const SYNTAX_ONLY = new Map([
  ...['bash', 'sh', 'zsh', 'dash', 'ksh'].map((shell) => [shell, new Set(['-n', '--noexec'])]),
  ['node', new Set(['--check', '-c'])],
  ['ruby', new Set(['-c'])],
]);
const WRITE_WORDS = new Set(['cp', 'mv', 'rm', 'touch', 'mkdir', 'tee', 'truncate', 'install']);
const WRITE_TARGET_FLAGS = /^(-|--|[a-zA-Z]=)/;
// Write targets that are never "outside the repository": scratch space and the kernel's own sinks.
const NEVER_OUTSIDE = [/^\/tmp(\/|$)/, /^\/var\/tmp(\/|$)/, /^\/dev\/(?:null|stdout|stderr|fd)\b/, /^\/run\/user\//];

// Some of the guest's read-only host mounts (boot.rs): the mothership's vm_dir at `/colonizer` and
// the agent's binaries, runner and plugins at `/opt/colonizer`. A write onto one is still the
// host's, so it asks. Matched by ancestor like a mount, and checked after the writable mounts, so a
// nested writable bind like `/colonizer/services` keeps the host-backed reason. Not exhaustive: a
// read-only bind added elsewhere in boot.rs is not listed here, and the bare repository's own mount
// lives at a host data-dir path this file cannot name.
const READ_ONLY_ROOTS = ['/colonizer', '/opt/colonizer'];

// Read-only host mounts of a single file (boot.rs mounts the vendored Claude Code and node
// runtimes read-only). Matched exactly, with a separator, so `/opt/node/bin/node_modules` is not
// `/opt/node/bin/node`.
const READ_ONLY_FILES = ['/opt/claude/bin/claude', '/opt/node/bin/node'];

// What a segment writes outside the checkout, ranked most severe first. `git` is a write into the
// checkout's own `.git`; `readonly` a write onto a [`READ_ONLY_ROOTS`]/[`READ_ONLY_FILES`] path;
// `host` a write onto a writable host mount (or, with no mount list at all, any absolute path
// outside the repo); and `vmlocal` a write into the microVM's discarded root filesystem — nothing
// asks for that by default (see #877), only a `"writes_outside": "strict"` rule does (#750).
const WRITE_SEVERITY = { git: 4, readonly: 3, host: 2, vmlocal: 1 };
const GIT_REASON = "the command writes into the repository's .git internals";
const HOST_REASON = 'the command writes to a host-backed path outside the repository';

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
// denied in the command and in any script it runs; writing to a host-backed path outside the
// repository asks (a write into the microVM's discarded root filesystem does not — see #877), as
// does a write onto a read-only host mount (`/colonizer`, `/opt/colonizer`) or into the checkout's
// own `.git` (#750). A layer can restore the pre-#877 strictness with `"writes_outside": "strict"`.
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
        reason: 'the command writes to a host-backed path outside the repository',
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
    if (raw.writes_outside === true || raw.writes_outside === 'strict') {
      const strict = raw.writes_outside === 'strict';
      rule.predicates.push({ kind: 'writes_outside', strict });
      rule.writes = strict ? 'strict' : 'outside'; // which per-path reason the hit reports
    }
    // A rule with none of the four known predicates would match every command by accident, so it
    // is dropped; a deliberate catch-all is `"command": "."` (or `""`).
    if (rule.predicates.length) rules.push(rule);
  }
  return { rules };
}

/**
 * The guest targets of the colony's writable host mounts, one absolute path per line (boot.rs
 * writes `vm_dir/host-mounts`). Blank lines and `#` comments are skipped; each path is normalized
 * and deduplicated. Every entry is a path the host still owns after the microVM dies — the only
 * place outside the repository a write can matter.
 * @param {string} text
 * @returns {string[]}
 */
export function parseHostMounts(text) {
  const mounts = [];
  const seen = new Set();
  for (const line of String(text ?? '').split('\n')) {
    const entry = line.trim();
    if (!entry || entry.startsWith('#') || !entry.startsWith('/')) continue;
    const path = normalize(entry).replace(/\/+$/, '') || '/';
    if (!seen.has(path)) {
      seen.add(path);
      mounts.push(path);
    }
  }
  return mounts;
}

/** The writable host-mount list the boot wrote, or null when the file is absent — a runner outside
 * a microVM, or a mothership predating the mount list. */
function loadHostMounts(env, readFile) {
  const file = env.COLONIZER_HOST_MOUNTS || HOST_MOUNTS_FILE;
  const text = readFile(file, HOST_MOUNTS_MAX_BYTES);
  return text ? parseHostMounts(text) : null;
}

/**
 * The colony's policy, layered: default → install → org → repo. Called once at runner start; the
 * repo file read here is the one the whole run keeps, so the agent rewriting it mid-run cannot
 * widen anything (and the layering would not let it anyway). A layer that does not parse is
 * dropped with a warning, never fatal: the default keeps enforcing. `hostMounts` (the writable
 * bind list, or null when unknown) rides on the result for [`evaluateExecPolicy`] to read.
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
  return {
    layers,
    warnings,
    hostMounts: loadHostMounts(env, readFile),
    trackedScripts: loadTrackedScripts(env, readFile),
  };
}

/**
 * The repository's own scripts at the base commit, path to git object id (#1239). The guest cannot
 * see that commit by itself, and the agent can rewrite the worktree's index and refs, so the host
 * writes this list at boot into the read-only `/colonizer` mount. Empty when absent.
 * @param {string} text
 * @returns {Map<string, string>}
 */
export function parseTrackedScripts(text) {
  const tracked = new Map();
  for (const line of String(text ?? '').split('\n')) {
    const match = line.match(/^([0-9a-f]{40}|[0-9a-f]{64}) (.+)$/);
    if (match) tracked.set(posix.normalize(match[2]), match[1]);
  }
  return tracked;
}

function loadTrackedScripts(env, readFile) {
  const file = env.COLONIZER_TRACKED_SCRIPTS || TRACKED_SCRIPTS_FILE;
  return parseTrackedScripts(readFile(file, TRACKED_SCRIPTS_MAX_BYTES));
}

/** The git object id of a blob with these bytes, SHA-1 or SHA-256 to match `id`'s length. */
export function gitBlobId(bytes, algorithm = 'sha1') {
  const body = Buffer.from(bytes);
  return createHash(algorithm).update(`blob ${body.length}\0`).update(body).digest('hex');
}

/** True when the script at `path` is one the base commit carries, with the same bytes. */
function isTrackedScript(path, text, cwd, tracked) {
  if (!tracked?.size || text.length >= SCRIPT_MAX_BYTES) return false; // a capped read cannot be hashed
  const rel = posix.relative(posix.normalize(cwd), posix.normalize(path));
  if (!rel || rel.startsWith('..') || rel.startsWith('/')) return false;
  const id = tracked.get(rel);
  return Boolean(id) && gitBlobId(text, id.length === 64 ? 'sha256' : 'sha1') === id;
}

/**
 * The decision for one Bash command, or null when no rule matches. Within a layer the first
 * matching rule wins; across layers the strictest decision wins, and on a tie the earlier layer's
 * rule is the one reported (the default names itself first, which is the one an operator reads).
 * @param {{ layers: {name: string, rules: object[]}[], hostMounts?: string[]|null }} policy
 *        as loadExecPolicy returned; its `hostMounts` narrows `writes_outside` (#877)
 * @param {string} command                                         the Bash tool's command
 * @param {object} [opts]   { cwd, readFile } — cwd is the repository root the command runs in
 * @returns {{ decision: string, rule: string, layer: string, reason: string } | null}
 */
export function evaluateExecPolicy(policy, command, opts = {}) {
  if (!policy || typeof command !== 'string' || !command) return null;
  const ctx = buildContext(command, opts, policy.hostMounts ?? null, policy.trackedScripts ?? null);
  let best = null;
  for (const layer of policy.layers) {
    for (const rule of layer.rules) {
      if (!ruleMatches(rule, ctx, layer.name)) continue;
      // A `writes_outside` rule reports WHY: the specific path classification the context found (a
      // read-only mount, the checkout's .git, a host-backed mount), falling back to the rule's own
      // reason for the plain strict case (a VM-local write, which no single path names).
      const reason = rule.writes === 'strict' ? ctx.strictWriteReason ?? rule.reason
        : rule.writes === 'outside' ? ctx.writeReason ?? rule.reason
        : rule.reason;
      const hit = { decision: rule.decision, rule: rule.id, layer: layer.name, reason };
      // A `touches` rule names the path it matched, as the command spelled it, so the boundary event
      // reports the refused file and not the first path-looking word (`cd /workspace && cat .env`
      // refuses `.env`, not the workspace root the watchdog would then see every call reach).
      const target = touchedWord(rule, ctx);
      if (target) hit.target = target;
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
function ruleMatches(rule, ctx, layer) {
  for (const p of rule.predicates) {
    switch (p.kind) {
      case 'command':
        if (!p.res.some((re) => re.test(ctx.command))) return false;
        break;
      case 'script': {
        // No readable script, no match: the rule is about what the command runs, and a command
        // that runs nothing readable cannot show it (the default layer's other rules still hold).
        // The repository's own scripts, byte for byte as the base commit has them, are not the
        // colony's to answer for (#1239): the default layer leaves them to the VM's network
        // policy. A layer an operator or the repository adds still sees them.
        const scripts = layer === 'default' ? ctx.scripts.filter((s) => !s.tracked) : ctx.scripts;
        if (!scripts.some((s) => p.res.some((re) => re.test(s.text)))) return false;
        break;
      }
      case 'touches':
        if (!ctx.tokens.some((t) => p.res.some((re) => re.test(t)) && !p.keepOut.some((re) => re.test(t)))) return false;
        break;
      case 'writes_outside':
        if (p.strict ? !ctx.writesOutsideAny : !ctx.writesOutside) return false;
        break;
    }
  }
  return true;
}

/** The word, as the command spelled it, that made a rule's `touches` predicate match, or null. */
function touchedWord(rule, ctx) {
  const p = rule.predicates.find((q) => q.kind === 'touches');
  if (!p) return null;
  const token = ctx.tokens.find((t) => p.res.some((re) => re.test(t)) && !p.keepOut.some((re) => re.test(t)));
  return token === undefined ? null : ctx.spelled.get(token) ?? token;
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

// tar and rsync `--exclude PATTERN` names what NOT to copy, so its pattern is not a path the command
// touches: excluding the placeholder dotfiles while copying a repository is the safe thing to do
// (#1169). Only the pattern itself goes; `--exclude-from FILE` reads FILE and keeps counting, and so
// does every other word of the segment.
const EXCLUDE_ARG = /(^|\s)--exclude(?:=|\s+)('[^']*'|"[^"]*"|[^\s;|&<>]+)/g;
function withoutExcludes(segment) {
  const first = String(segment ?? '').trim().split(/\s+/).find((w) => !/^[A-Za-z_][A-Za-z0-9_]*=/.test(w) && w !== 'sudo' && w !== 'env');
  const bin = (first ?? '').split('/').pop();
  return bin === 'tar' || bin === 'rsync' ? segment.replace(EXCLUDE_ARG, '$1') : segment;
}

/** The words of one command segment, cleaned of the punctuation that glues paths to words. */
function words(text) {
  return String(text ?? '')
    .replace(/\$\{HOME\}/g, '$HOME') // before the split: the braces would break the word apart
    .split(/[\s;|&<>()"'`\[\]{}]+/)
    .map((w) => w.replace(/^[@:=+]+/, '').replace(/[,;:]+$/, ''))
    // One-char words are noise (`echo x`), except the filesystem root, which a `rm -rf /` writes.
    .filter((w) => w.length > 1 || w === '/');
}

/**
 * The paths a git segment names inside `<rev>:<path>` object specs (#1241): `git show HEAD:.env`,
 * `git cat-file -p origin/main:.npmrc` or `<sha>:config/.netrc` read the file as committed, and the
 * `<rev>:` prefix would otherwise hide it from a `touches` glob. Any git subcommand counts; a word
 * without a colon, or with only a leading one (`:.env`, which [`words`] already strips), adds
 * nothing.
 */
function gitObjectPaths(segment) {
  if (basename(commandWords(segment)[0] ?? '') !== 'git') return [];
  return words(segment)
    .filter((w) => w.indexOf(':') > 0)
    .map((w) => w.slice(w.indexOf(':') + 1))
    .filter((path) => path.length > 1);
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
    const flags = [];
    for (const w of ws.slice(1)) {
      if (!w.startsWith('-')) {
        // A syntax check reads the script and runs none of it (#1227).
        const syntax = SYNTAX_ONLY.get(basename(head));
        if (syntax && flags.length && flags.every((f) => syntax.has(f))) return null;
        return expandTilde(w); // the first non-flag argument is the script
      }
      flags.push(w);
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

/** True when `target` sits at, under, or above one of `mounts` (guest targets). Comparisons are on
 * segment boundaries, so `/root/.claudefoo` is not under a mounted `/root/.claude`. The ancestor
 * case is deliberate: `rm -rf /root` and `rm -rf /` ask while a path below them is mounted. */
function onMount(target, mounts) {
  const parts = normalize(target).split('/').filter(Boolean);
  return mounts.some((mount) => {
    const mountParts = normalize(mount).split('/').filter(Boolean);
    const shared = Math.min(parts.length, mountParts.length);
    return parts.slice(0, shared).join('/') === mountParts.slice(0, shared).join('/');
  });
}

/** True when `target` is exactly `path` or sits under it (`path` + `/`), separator-safe. */
function underPath(target, path) {
  return target === path || target.startsWith(`${path}/`);
}

/** True when `target` (absolute, normalized) lands in the checkout's own `.git`. */
function inGitDir(target, cwd) {
  return underPath(target, `${cwd}/.git`);
}

/**
 * What one normalized absolute write target is, or null when it is none of the policy's business: a
 * normal repository write, or a kernel sink (`/tmp`, `/dev/null`). A write into the checkout's
 * `.git`, or onto a read-only host path, is the host's whichever way `hostMounts` reads. With
 * `hostMounts` known, a path on a writable mount is `host` and anything else outside the repo is
 * `vmlocal` (the microVM's discarded root filesystem); with no mount list (an older mothership, or
 * a runner outside a VM) every path outside the repo reads as `host`, the conservative pre-#877
 * answer. `cwd` is the normalized repository root.
 */
function classifyTarget(target, cwd, hostMounts) {
  if (inGitDir(target, cwd)) return { kind: 'git', reason: GIT_REASON };
  if (underPath(target, cwd)) return null; // a normal repo write
  if (NEVER_OUTSIDE.some((re) => re.test(target))) return null;
  if (hostMounts && onMount(target, hostMounts)) return { kind: 'host', reason: HOST_REASON };
  const root = READ_ONLY_ROOTS.find((mount) => onMount(target, [mount]));
  if (root) return { kind: 'readonly', reason: `the command writes to a read-only mount (${root})` };
  const file = READ_ONLY_FILES.find((path) => underPath(target, path));
  if (file) return { kind: 'readonly', reason: `the command writes to a read-only mount (${file})` };
  return hostMounts ? { kind: 'vmlocal' } : { kind: 'host', reason: HOST_REASON };
}

/**
 * The most severe classification among a segment's write targets, or null. Absolute targets are
 * normalized first, so `/w/sub/../.git/x` reads as the git path it is and `/w/../etc/passwd` as the
 * outside path it is. Relative targets are resolved against the checkout and flagged only when they
 * land in its `.git` (`echo x > .git/HEAD` asks); any other relative target stays inside, as before,
 * so a `../x` write does not start asking.
 */
function classifyWrite(segment, cwd, hostMounts) {
  const root = posix.normalize(cwd);
  let best = null;
  for (const raw of writeTargets(segment)) {
    const target = expandTilde(raw);
    const hit = target.startsWith('/')
      ? classifyTarget(posix.normalize(target), root, hostMounts)
      : inGitDir(posix.join(root, target), root) ? { kind: 'git', reason: GIT_REASON } : null;
    if (hit && (!best || WRITE_SEVERITY[hit.kind] > WRITE_SEVERITY[best.kind])) best = hit;
  }
  return best;
}

// --- name-only commands (#1169) ------------------------------------------------------------

// Commands that can only learn a path's NAME, existence or size, never its bytes: `git
// check-ignore`, `git status`, `ls`, `stat`, `wc -c`, `test -e|-f|-d|-s|...` and `[ -f P ]`. The
// path policy's empty placeholders (.env, .netrc, ...) show up to an agent as dotfiles, and asking
// git whether one is ignored, or how big it is, is not an attempt to read a secret, so such a
// segment does not feed the `touches` predicate.
const SHORT_FLAGS = /^-[A-Za-z]+$/;
const LONG_FLAGS = /^--[a-z][a-z-]*$/; // no `=value`: nothing here takes an argument worth smuggling
const NUMBER = /^-?\d+$/;
// A `stat` format: conversions and plain punctuation, never a path or an expansion.
const STAT_FORMAT = /^[%A-Za-z0-9_.:,+=-]+$/;
// `test` and `[` operators that look at a path's metadata only.
const FILE_TESTS = ['-e', '-f', '-d', '-s', '-r', '-w', '-x', '-L', '-h'];
// Filters that, given no path, only read their stdin: what follows a pipe from a name-only
// command, so `git check-ignore -v .env | head` sees names and nothing else.
const STDIN_FILTERS = new Set(['head', 'tail', 'sort', 'uniq', 'wc']);
// Anything that can feed or redirect bytes, or run another command, and so turn a name-only
// segment into a read: pipes, redirects, substitutions, subshells, braces and backslash escapes.
const UNSAFE_SHELL = /[|<>`$(){}\\]/;
// The same, minus the pipe: a pipe is checked segment by segment (only into a STDIN_FILTERS
// command with no path of its own).
const UNSAFE_SHELL_PIPED = /[<>`$(){}\\]/;
// Redirects that move no bytes into or out of a file: stderr onto stdout, or output thrown away.
const HARMLESS_REDIRECT = /(^|\s)(?:2>&1|[12]?>\s*\/dev\/null|&>\s*\/dev\/null)(?=\s|;|\||&|$)/g;

/** True when one segment (already split on `;`, `&&` and the like) only asks about path names. */
function isNameOnlySegment(segment, cwd) {
  const ws = segment.trim().split(/\s+/).map((w) => w.replace(/^["']|["']$/g, ''));
  if (!ws.length || ws.some((w) => /["']/.test(w))) return false;
  const paths = (rest) => rest.every((w) => !w.startsWith('-'));
  const flags = (rest) => rest.filter((w) => w.startsWith('-')).every((w) => SHORT_FLAGS.test(w) || LONG_FLAGS.test(w));
  if (ws[0] === 'git') {
    // `-C <dir>` is allowed only when it names the repository the command already runs in (#1079
    // follow-up: `git -C /workspace check-ignore --no-index ...`); another directory could carry a
    // config whose hooks run. Then the subcommand must come first: `git -c core.pager=... status`
    // could run a command.
    let i = 1;
    while (ws[i] === '-C') {
      const dir = ws[i + 1] ?? '';
      if (!(dir === '.' || (cwd && dir.startsWith('/') && posix.normalize(dir).replace(/\/+$/, '') === posix.normalize(cwd).replace(/\/+$/, '')))) return false;
      i += 2;
    }
    if (!['check-ignore', 'status'].includes(ws[i])) return false;
    return flags(ws.slice(i + 1));
  }
  if (ws[0] === 'ls') return flags(ws.slice(1));
  // `echo` of literal words (the caller already refused every expansion) only prints its own text.
  if (ws[0] === 'echo') return true;
  // `wc -c` reports a size, the same thing `ls -l` shows; `-l`, `-w` and `-m` count what is inside.
  if (ws[0] === 'wc') {
    const rest = ws.slice(1);
    return rest.some((w) => w.startsWith('-')) && rest.filter((w) => w.startsWith('-')).every((w) => w === '-c' || w === '--bytes');
  }
  if (ws[0] === 'stat') return statArgs(ws.slice(1));
  if (ws[0] === 'test') return ws.length === 3 && FILE_TESTS.includes(ws[1]) && paths([ws[2]]);
  if (ws[0] === '[') return ws.length === 4 && FILE_TESTS.includes(ws[1]) && paths([ws[2]]) && ws[3] === ']';
  return false;
}

/** `stat` arguments: flags, an optional `-c`/`--format`/`--printf` format, and paths. */
function statArgs(rest) {
  for (let i = 0; i < rest.length; i++) {
    const w = rest[i];
    if (w === '-c' || w === '--format' || w === '--printf') {
      if (!STAT_FORMAT.test(rest[i + 1] ?? '')) return false;
      i++;
    } else if (/^--(?:format|printf)=/.test(w)) {
      if (!STAT_FORMAT.test(w.replace(/^--(?:format|printf)=/, ''))) return false;
    } else if (w.startsWith('-') && !SHORT_FLAGS.test(w) && !LONG_FLAGS.test(w)) {
      return false;
    }
  }
  return true;
}

/** True when a segment is a stdin filter given no path: `head`, `head -n 5`, `sort -u`, `wc -l`. */
function isStdinFilter(segment) {
  const ws = segment.trim().split(/\s+/);
  return STDIN_FILTERS.has(ws[0]) && ws.slice(1).every((w) => SHORT_FLAGS.test(w) || LONG_FLAGS.test(w) || NUMBER.test(w));
}

/**
 * The segments whose words feed the `touches` predicate: all of them, minus the name-only ones,
 * when the whole command is plain (no pipe, redirect, substitution or subshell anywhere, so no
 * other segment can consume what a name-only one prints, and nothing hides bytes in a segment).
 * The one loop the incident ran, `for f in .env .netrc; do git check-ignore -v "$f"; done`, counts
 * as name-only when its body is. Anything touching `.ssh` keeps counting; any other segment, such
 * as `cat .env`, keeps its tokens and is refused as before.
 */
function nameOnlyFiltered(command, segments, cwd) {
  if (/\.ssh/.test(command)) return segments;
  // One line only: a newline inside the body would hide a second command (`hash -p /bin/cat ls`).
  const loop = !/[\r\n]/.test(command.trim()) && command.trim().match(/^for\s+([A-Za-z_]\w*)\s+in\s+([^;|&<>`$(){}\\]+);\s*do\s+([^;|&<>`(){}\\]+?);?\s*done$/);
  if (loop) {
    const [, name, , body] = loop;
    const ref = new RegExp(`"?\\$(?:${name}|\\{${name}\\})"?`, 'g');
    const plain = body.replace(ref, 'X');
    return !UNSAFE_SHELL.test(plain) && isNameOnlySegment(plain, cwd) ? [] : segments;
  }
  if (!UNSAFE_SHELL.test(command)) {
    // All or nothing: one other segment (`alias ls=cat`, `export PATH=...`, `hash -p /bin/cat ls`)
    // can change what a later `ls` or `test` runs, so only a command made of name-only segments
    // and nothing else drops its tokens.
    return segments.every((segment) => !segment.trim() || isNameOnlySegment(segment, cwd)) ? [] : segments;
  }
  // The shape agents reach for to keep output short: `git check-ignore -v .env 2>&1 | head; wc -c
  // .env 2>&1 | head`. With `2>&1` and `>/dev/null` gone, and no other redirect, substitution or
  // escape left, every segment must be name-only or a stdin filter with no path of its own (`head`,
  // not `head .env`, and never `cat` or `xargs`), so what crosses a pipe is names and sizes.
  const plain = command.replace(HARMLESS_REDIRECT, '$1');
  if (/[\r\n]/.test(plain.trim()) || UNSAFE_SHELL_PIPED.test(plain)) return segments;
  const parts = splitCommands(plain);
  return parts.length && parts.every((segment) => isNameOnlySegment(segment, cwd) || isStdinFilter(segment)) ? [] : segments;
}

/**
 * Everything the predicates see, derived from the command alone: its segments, the tokens the
 * `touches` globs match against (the command's words and every script's words), the scripts it
 * runs (resolved against the repo root, contents capped), and what it writes outside the checkout.
 * `writesOutside` is the default predicate: a git, read-only or host-backed write (with no
 * `hostMounts`, any path outside the repo). `writesOutsideAny` is the strict predicate (#750): any
 * of those plus a write into the microVM's own root filesystem. Each carries the reason for the
 * write it matched; a VM-local write has no reason of its own, so a strict rule names it.
 */
function buildContext(command, { cwd = process.cwd(), readFile = readScriptFile } = {}, hostMounts = null, tracked = null) {
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
    if (text) scripts.push({ path, text, tracked: isTrackedScript(path, text, cwd, tracked) });
  }
  const tokens = new Set();
  const spelled = new Map(); // token -> the word as the command spelled it (`~/.ssh`, not /root/.ssh)
  for (const word of [...nameOnlyFiltered(command, segments, cwd).map(withoutExcludes).flatMap((s) => [...words(s), ...gitObjectPaths(s)]), ...scripts.flatMap((s) => words(s.text))]) {
    const token = expandTilde(word);
    tokens.add(token);
    if (!spelled.has(token)) spelled.set(token, word);
  }
  const writes = segments.map((segment) => classifyWrite(segment, cwd, hostMounts)).filter(Boolean);
  const pick = (kinds) => writes
    .filter((w) => kinds.includes(w.kind))
    .reduce((best, w) => (!best || WRITE_SEVERITY[w.kind] > WRITE_SEVERITY[best.kind] ? w : best), null);
  const outside = pick(['git', 'readonly', 'host']);
  const strict = pick(['git', 'readonly', 'host', 'vmlocal']);
  return {
    command,
    segments,
    tokens: [...tokens],
    spelled,
    scripts,
    writesOutside: outside !== null,
    writesOutsideAny: strict !== null,
    writeReason: outside?.reason ?? null,
    strictWriteReason: strict?.reason ?? null,
  };
}
