#!/usr/bin/env node
// Colonizer agent runner for Claude Code. Implements the runner contract in docs/protocol.md §2:
// commands arrive as JSON lines on stdin, protocol events leave as JSON lines on stdout.
// Diagnostics go to stderr only.

import { execFile } from 'node:child_process';
import { closeSync, existsSync, fstatSync, openSync, readFileSync, readSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

import { annotateDenial, classifyDenial, denialGuidance } from './denials.mjs';
import {
  EXEC_POLICY_QUESTION_KIND,
  createExecAllowCache,
  evaluateExecPolicy,
  execPolicyLogLine,
  execPolicyQuestion,
  execPolicyReason,
  loadExecPolicy,
} from './execpolicy.mjs';
import { evaluatePathPolicy, loadPathPolicy } from './pathpolicy.mjs';
import { COORDINATION_PROMPT_APPEND, COORDINATION_SERVER, createCoordinationServer } from './coordinate.mjs';
import { createFindingsServer, FINDINGS_PROMPT_APPEND, FINDINGS_SERVER, findingDecision } from './findings.mjs';
import { createGithubServer, GITHUB_PROMPT_APPEND, GITHUB_SERVER, githubDecision } from './github.mjs';
import { ConditionalInstructions, PATH_TOOLS_MATCHER, parseLabels } from './instructions.mjs';
import { createLoopServer, LOOP_SERVER, loopDecision, loopPromptAppend } from './loop.mjs';
import { createMemoryServer, MEMORY_PROMPT_APPEND, MEMORY_SERVER, memoryDecision } from './memory.mjs';
import { createWaitServer, WAIT_PROMPT_APPEND, WAIT_SERVER } from './wait.mjs';
import { startHeadroom } from './headroom.mjs';
import { runPreflight, shouldBlock } from './preflight.mjs';
import { createRecallServer, RECALL_PROMPT_APPEND, RECALL_SERVER } from './recall.mjs';
import { routeEnv, routingPlan, startRouter } from './router.mjs';
import { subagentDefinitions } from './subagents.mjs';

export const SYSTEM_PROMPT_APPEND = [
  'You are running inside the Colonizer; the user follows along in a web UI.',
  '- Whenever you need a decision, a clarification or any other input from the user, call the AskUserQuestion tool with 2-4 concrete options. Never ask the user in plain text, and never end a turn with a plain-text question. The UI always adds a free-text "Other" choice, so do not add one yourself.',
  '- Do not run `git commit` or `git push` and do not create branches; the harness commits your changes and opens the pull request.',
].join('\n');

export const DELEGATE_PROMPT_APPEND = [
  '- You are the orchestrator of this colony. Plan the work, split it into tasks, and start a subagent with the Task tool for each one. Read the subagent\'s report, decide what follows, and keep a subagent going until its task is genuinely done.',
  '- Do the thinking yourself: what to build, in what order, whether a result is good enough, and what to tell the user. Leave reading, searching, editing, running commands and tests to subagents.',
  '- Ask each subagent for a focused report: its conclusions, file:line references for every claim, and verbatim snippets only where you need the exact text. Do not ask for exhaustive, verbatim or "in full" dumps of files; everything a report carries stays in your context for the rest of the colony. When you need more detail on one point, start another subagent for it.',
  '- A subagent started in the background reports back on its own. While you wait, start other work or end your turn; when nothing is left but the waiting, mcp__colonizer_wait__wait holds the turn — never a placeholder command such as sleep or echo.',
  '- Subagents do not see this system prompt. When the colony\'s limits below bear on a task, include them in that subagent\'s brief.',
].join('\n');

/**
 * The enforced boundary, in the model's own words. Claude Code derives a Task subagent's tool pool
 * from the session's own, so the tools the gate refuses cannot be removed from the orchestrator's
 * list without removing them from the subagents doing the work; naming the boundary here is what
 * stops the model rediscovering it one refused call at a time.
 */
export const ENFORCE_PROMPT_APPEND = [
  '- Delegation is enforced in this colony: the only tools you may call yourself are Task (or Agent) to start a subagent, SendMessage, ListAgents and TaskStop to direct the ones you started, AskUserQuestion, TodoWrite, EnterPlanMode and ExitPlanMode, Skill to load a skill\'s instructions, the colony\'s mcp__colonizer_* tools, and Write to /harness/out/pr.md.',
  '- Every other tool you can see — Bash and Read included — belongs to your subagents. Calling one yourself is refused and costs a turn, so hand that work to a subagent instead.',
].join('\n');

/** Lockfile -> package manager, in the order trusted when a repo carries several (verify.rs's JS_LOCKFILES). */
export const JS_LOCKFILES = [
  ['bun.lock', 'bun'],
  ['bun.lockb', 'bun'],
  ['pnpm-lock.yaml', 'pnpm'],
  ['yarn.lock', 'yarn'],
  ['package-lock.json', 'npm'],
  ['npm-shrinkwrap.json', 'npm'],
];

/** package.json's `packageManager` field (corepack's `name@version`), limited to the managers verify.rs knows. */
function pinnedManager(packageJson) {
  let value;
  try {
    value = JSON.parse(packageJson).packageManager;
  } catch {
    return null; // unreadable/invalid package.json just means the field cannot settle it
  }
  if (typeof value !== 'string') return null;
  const pinned = value.trim();
  const at = pinned.indexOf('@');
  if (at < 0) return null; // corepack's form always carries a version; verify.rs's split_once('@') requires one
  const name = pinned.slice(0, at);
  return ['npm', 'pnpm', 'yarn', 'bun'].includes(name) ? name : null;
}

/**
 * The package manager of the repository checkout `dir`, decided as the done-claim verifier decides it (verify.rs):
 * package.json's `packageManager` field, else the first lockfile of [`JS_LOCKFILES`], else npm. Returns
 * `{ name, source }`, or null when there is no package.json — a lockfile alone is not a JavaScript repo, as verify.rs
 * reads it. Sync and failure-tolerant: never throws.
 */
export function packageManager(dir) {
  const packageJson = readText(join(dir, 'package.json'));
  if (packageJson === null) return null;
  const pinned = pinnedManager(packageJson);
  if (pinned) return { name: pinned, source: 'packageManager' };
  const lock = JS_LOCKFILES.find(([file]) => existsSync(join(dir, file)));
  return lock ? { name: lock[1], source: lock[0] } : { name: 'npm', source: 'package.json' };
}

/**
 * What the colony can and cannot reach, so no model spends a turn discovering it. `image` is the container image the
 * mothership booted (COLONIZER_IMAGE); `manager` is [`packageManager`] for the checkout, when one was found.
 */
export function environmentPrompt(image, manager = null) {
  const bullets = [
    '- This colony has no GitHub access: there is no gh CLI and no GitHub credentials, so the GitHub API and private repositories are out of reach. The issue is already in your brief, and the harness publishes the pull request.',
    `- The colony runs the container image \`${image}\`. Toolchains it does not include (a Rust or Swift toolchain in a Node image, for example) are not installed. Check once with \`command -v\` before relying on one. Install a toolchain only when the task genuinely needs it to build or test; otherwise say in your report what could not be run.`,
  ];
  if (manager) {
    bullets.push(
      `- This repository uses ${manager.name} (from \`${manager.source}\`): install and test with \`${manager.name}\` rather than another package manager; done-claim verification runs it the same way.`,
    );
  }
  return bullets.join('\n');
}

/**
 * The SDK's per-model usage, reduced to what a colony's cost is made of. Keys are the model names the colony used:
 * routed providers keep their `provider/model` form, and Claude models have no slash. Claude Code prices a model it
 * does not know at the main model's rate, so only Claude models' estimates are summed into `claudeCostUsd`; the rest
 * are reported as tokens.
 */
export function summariseUsage(modelUsage) {
  if (!modelUsage || typeof modelUsage !== 'object') return null;
  const models = {};
  let claudeCostUsd = 0;
  for (const [model, u] of Object.entries(modelUsage)) {
    if (!u || typeof u !== 'object') continue;
    const count = (value) => (Number.isFinite(value) ? value : 0);
    models[model] = {
      input_tokens: count(u.inputTokens),
      output_tokens: count(u.outputTokens),
      cache_read_tokens: count(u.cacheReadInputTokens),
      cache_write_tokens: count(u.cacheCreationInputTokens),
    };
    if (!model.includes('/')) claudeCostUsd += count(u.costUSD);
  }
  return Object.keys(models).length ? { models, claudeCostUsd } : null;
}

/** Where a superpowers plugin keeps the skill its SessionStart hook injects. */
export const SUPERPOWERS_SKILL = 'skills/using-superpowers/SKILL.md';

/**
 * Other superpowers skills name the two that aren't staged (scripts/fetch-vendor.sh); this says why they are
 * missing and what to do at those steps instead.
 */
export const SUPERPOWERS_COLONIZER_NOTE = [
  "- superpowers' using-git-worktrees and finishing-a-development-branch skills are not installed in this colony, on purpose. You already work in an isolated git worktree on the branch the harness publishes, so a step that asks for an isolated workspace is already done.",
  '- Where a skill tells you to use finishing-a-development-branch, or to merge, push or open a pull request, stop at that step: the harness commits your changes and opens the pull request.',
].join('\n');

/**
 * superpowers switches itself on with a SessionStart hook that injects its using-superpowers skill. Colonizer
 * doesn't run plugin hooks, so the same text goes into the system prompt, which also survives compaction
 * (the hook re-ran on `compact`). The wrapper is the hook's own.
 */
export function superpowersBootstrap(skill) {
  return [
    '<EXTREMELY_IMPORTANT>',
    'You have superpowers.',
    '',
    "**Below is the full content of your 'superpowers:using-superpowers' skill - your introduction to using skills. For all other skills, use the 'Skill' tool:**",
    '',
    skill,
    '</EXTREMELY_IMPORTANT>',
    SUPERPOWERS_COLONIZER_NOTE,
  ].join('\n');
}

/** Where the mothership mounts what the token-saving settings need (crates/colonizer/src/sessions.rs). */
export const RTK_BIN = '/opt/colonizer/bin/rtk';
export const CAVEMAN_SKILL = '/opt/colonizer/caveman/SKILL.md';
export const CAVEMAN_LEVELS = new Set(['lite', 'full', 'ultra']);
// Jev compaction loads as a plugin, like the skill dirs: the mothership mounts it read-only here,
// and its SDK id for a local plugin dir is the name plus @inline.
export const JEV_PLUGIN = 'fast-jev-compaction';
export const JEV_PLUGIN_ID = `${JEV_PLUGIN}@inline`;
export const JEV_COMPACTION_DIR = '/opt/colonizer/jev-compaction';

/** True when buildOptions switched Jev compaction on: its plugin config is in the SDK settings. */
export function jevEnabled(options) {
  return Boolean(options?.settings?.pluginConfigs?.[JEV_PLUGIN_ID]);
}

/** Dotted-version compare against 2.1.274 (function hooks), tolerating suffixes like "2.1.280 (Claude Code)". */
export function jevVersionOk(version) {
  const parts = String(version ?? '').match(/\d+(\.\d+)*/)?.[0].split('.').map(Number) ?? [];
  for (const [i, want] of [2, 1, 274].entries()) {
    if ((parts[i] ?? 0) !== want) return (parts[i] ?? 0) > want;
  }
  return true;
}

/** The last Jev verdict in debug-log text, stripped of any log prefix, or null when there is none. */
export function latestJevVerdict(text) {
  let verdict = null;
  for (const line of String(text ?? '').split('\n')) {
    const match = line.match(/kept \d+\/\d+ messages, no summary.*|fallback to built-in summary.*/);
    if (match) verdict = match[0].trim();
  }
  return verdict;
}

/**
 * Jev's per-pair keep/drop decisions, parsed from debug-log text. Each pass logs one contiguous
 * `decisions:` group, chunked at 4096 characters with continuation lines prefixed `decisions (i/n): `
 * — one prefix family, stripped here — and there is one pass per debounced read window, so like
 * latestJevVerdict only the last group in the window counts. A token is
 * `t{n}:{tool}:{action}/call={c}/result={r}`, where t{n} indexes the transcript's
 * tool-use/tool-result pairs 1-based and pinned recent pairs get no token at all, so `n` has gaps.
 */
export function parseJevDecisions(text) {
  let group = [];
  let contiguous = false;
  for (const line of String(text ?? '').split('\n')) {
    const match = line.match(/decisions(?: \(\d+\/\d+\))?:\s*(.*)/);
    if (match) {
      if (!contiguous) group = [];
      contiguous = true;
      group.push(...match[1].split(/\s+/).filter(Boolean));
    } else {
      contiguous = false;
    }
  }
  const decisions = [];
  for (const token of group) {
    const d = token.match(/^t(\d+):([^:/]+):(keep|drop_result|drop_call)\/call=([\d.]+)\/result=([\d.]+)$/);
    if (d) decisions.push({ n: Number(d[1]), tool: d[2], action: d[3], keepCall: Number(d[4]), keepResult: Number(d[5]) });
  }
  return decisions;
}

/**
 * caveman's ruleset for the system prompt. The plugin switches itself on with SessionStart and
 * UserPromptSubmit hooks that inject this text and track a per-session level; in a colony the level is a
 * setting and the text goes straight into the prompt. What a colony writes for people other than the
 * user in the chat stays in plain sentences.
 */
export function cavemanPrompt(skill, level) {
  const body = skill.replace(/^---\n[\s\S]*?\n---\n/, '').trim();
  return [
    `Caveman mode is on for this colony at the ${level} level. The level is set in Colonizer's settings, so ignore the /caveman switch the rules mention.`,
    '',
    body,
    '',
    '- Colonizer exceptions: write the pull request description, AskUserQuestion questions and options, memory proposals and code comments in plain, complete sentences. Caveman style is for your replies in the chat.',
  ].join('\n');
}

/**
 * The command `rtk rewrite` turns `command` into, or null to run it unchanged. rtk's exit codes: 0 with
 * output, a rewrite; 3 with output, a rewrite its ask rules flag (the colony's own permission handling
 * still applies); 1, no rtk equivalent; 2, a deny rule matched. Anything else, including rtk missing or
 * slow, leaves the command alone: saving tokens must never break a command.
 */
export function rtkRewrite(command, bin = RTK_BIN) {
  return new Promise((resolve) => {
    execFile(bin, ['rewrite', command], { encoding: 'utf8', timeout: 2000 }, (error, stdout) => {
      const status = error ? error.code : 0;
      if ((status !== 0 && status !== 3) || typeof stdout !== 'string') return resolve(null);
      const rewritten = stdout.replace(/\r?\n$/, '');
      resolve(rewritten && rewritten !== command ? rewritten : null);
    });
  });
}

function readText(path) {
  try {
    return readFileSync(path, 'utf8');
  } catch {
    return null;
  }
}

export const CHOICE_NUDGE =
  'You ended your turn with a question in plain text. Ask it again with the AskUserQuestion tool, offering 2-4 concrete options, and wait for the answer.';

/** True when a turn's final text ends by asking the user something. */
export function endsWithQuestion(text) {
  if (typeof text !== 'string') return false;
  return /\?[\s*_`'")\]]*$/.test(text.trim());
}

export const MAX_TOOL_OUTPUT = 20_000;
const ASK_TOOL = 'AskUserQuestion';

/**
 * Tools the orchestrator keeps when delegation is enforced, grouped by why each is not subagent work.
 * Everything else is refused by delegationDecision, including tools the harness's own text invites.
 */
export const ORCHESTRATOR_TOOLS = new Set([
  // Delegating: `Task` is Claude Code's legacy alias for `Agent`, and sessions use both spellings.
  'Task',
  'Agent',
  // Directing the subagents it already started. The harness's own prompt and the Agent tool's result
  // text hand back an agentId to continue with SendMessage, so while the gate refused them SendMessage
  // failed 98.5% of its calls (issue #182) — the invitation and the gate have to agree.
  'SendMessage',
  'ListAgents',
  'TaskStop',
  // Asking the user and keeping its own plan visible.
  ASK_TOOL,
  'TodoWrite',
  // Planning: ExitPlanMode was allowed without EnterPlanMode, which was an oversight.
  'EnterPlanMode',
  'ExitPlanMode',
  // Loading a skill: it only brings instructions into the context, which is guidance for planning
  // rather than the work itself. A skill that says to run or edit something still meets this gate
  // on every tool it names, and its allowed-tools only pre-approve calls the hook refuses anyway.
  // The superpowers bootstrap tells the orchestrator to use it (issue #188).
  'Skill',
]);

/** The one thing the orchestrator writes itself; the harness reads it to open the pull request. */
const OUT_DIR = '/harness/out';

/**
 * Why a tool call is refused when `delegate = enforce`, or null when it is allowed.
 *
 * Only the main thread is constrained: a subagent's own calls carry `agent_id` in the hook input, and
 * subagents doing the work is the entire point. Unknown tools are refused rather than allowed, so a
 * tool added later cannot quietly become an orchestrator shortcut.
 *
 * The notable refusals stay refused on purpose. Read, Grep, Glob, WebFetch and WebSearch are not
 * beyond the orchestrator, but everything they return stays in its context for the rest of the
 * colony, and a subagent's focused report is the cheaper way in — DELEGATE_PROMPT_APPEND already
 * makes that argument. Bash, Edit, Write and NotebookEdit are the work itself, which is what
 * subagents are for (Write, Edit and Read under /harness/out stay allowed for the pull request
 * description). TaskOutput returns a subagent's raw transcript, the same context blow-up by another
 * route, and a background subagent reports on its own.
 */
export function delegationDecision(toolName, toolInput = {}, hookInput = {}) {
  if (hookInput.agent_id) return null;
  if (ORCHESTRATOR_TOOLS.has(toolName) || toolName.startsWith('mcp__')) return null;
  const path = typeof toolInput?.file_path === 'string' ? toolInput.file_path : '';
  if (path.startsWith(OUT_DIR)) return null;
  return (
    `${toolName} belongs to your subagents in this colony. Start one with the Task tool and have it do this; ` +
    `you plan, decide and review. ${OUT_DIR}/pr.md is yours to write.`
  );
}
const EFFORT_LEVELS = new Set(['low', 'medium', 'high', 'xhigh', 'max']);
// Claude Code variables that mean "you are nested inside another Claude Code session".
const KEEP_CLAUDE_CODE_VARS = /^CLAUDE_CODE_(OAUTH_TOKEN|MAX_RETRIES|USE_BEDROCK|USE_VERTEX|USE_FOUNDRY)$/;

/** Minimal async queue usable as an AsyncIterable (SDK prompt stream and stdin commands). */
export class AsyncQueue {
  #items = [];
  #waiters = [];
  #closed = false;

  push(value) {
    if (this.#closed) return false;
    const waiter = this.#waiters.shift();
    if (waiter) waiter({ value, done: false });
    else this.#items.push(value);
    return true;
  }

  close() {
    this.#closed = true;
    for (const waiter of this.#waiters.splice(0)) waiter({ value: undefined, done: true });
  }

  [Symbol.asyncIterator]() {
    return {
      next: () => {
        if (this.#items.length) return Promise.resolve({ value: this.#items.shift(), done: false });
        if (this.#closed) return Promise.resolve({ value: undefined, done: true });
        return new Promise((resolve) => this.#waiters.push(resolve));
      },
      return: () => {
        this.close();
        return Promise.resolve({ value: undefined, done: true });
      },
    };
  }
}

/** AskUserQuestion input → protocol `questions`. */
export function normalizeQuestions(input) {
  const questions = Array.isArray(input?.questions) ? input.questions : [];
  return questions.map((q) => ({
    question: String(q?.question ?? ''),
    header: String(q?.header ?? ''),
    multi_select: Boolean(q?.multiSelect),
    options: (Array.isArray(q?.options) ? q.options : []).map((o) => ({
      label: String(o?.label ?? ''),
      description: String(o?.description ?? ''),
      preview: typeof o?.preview === 'string' ? o.preview : null,
    })),
  }));
}

/**
 * The risk class a question is published with, so the mothership can route it: an answering judge is
 * configured with a ceiling and only sees questions at or below it, so misclassifying upward at worst
 * costs the latency of a human, while misclassifying down would hand a credential- or publish-shaped
 * question to a judge that must never see it. The heuristic therefore only rounds up, matches on word
 * boundaries (a tokenizer is not a token), and `read_only` stays in the vocabulary for emitters that
 * know more than this scan does — it is never a heuristic verdict.
 */
const CREDENTIALISH =
  /\b(credentials?|secrets?|passwords?|passphrases?|(?:api|private|ssh)[\s_-]?keys?|tokens?|oauth)\b|\.env\b/i;
const PUBLISHISH =
  /\b(publish(?:es|ed|ing)?|releas(?:e|es|ed|ing)|deploy(?:s|ed|ing|ment)?|push(?:es|ed|ing)?|merg(?:e|es|ed|ing)|pull[\s_-]?requests?)\b/i;

/** Round-up risk class of a normalized `questions` array; highest class across all its text wins. */
export function riskClass(questions) {
  const text = (Array.isArray(questions) ? questions : [])
    .flatMap((q) => [
      q?.question,
      q?.header,
      ...(Array.isArray(q?.options) ? q.options : []).map((o) => `${o?.label}\n${o?.description}`),
    ])
    .join('\n');
  if (CREDENTIALISH.test(text)) return 'credential_adjacent';
  if (PUBLISHISH.test(text)) return 'publish_affecting';
  return 'workspace_write';
}

/** tool_result content → display text, capped at MAX_TOOL_OUTPUT characters. */
export function toolResultText(content) {
  let text;
  if (typeof content === 'string') text = content;
  else if (Array.isArray(content)) {
    text = content
      .map((part) => (part?.type === 'text' ? part.text : part?.type === 'image' ? '[image]' : JSON.stringify(part)))
      .join('\n');
  } else text = content == null ? '' : JSON.stringify(content);
  if (text.length <= MAX_TOOL_OUTPUT) return text;
  const suffix = `\n… [truncated ${text.length - MAX_TOOL_OUTPUT} characters]`;
  return text.slice(0, MAX_TOOL_OUTPUT - suffix.length) + suffix;
}

export function childEnv(env) {
  const out = {};
  for (const [key, value] of Object.entries(env)) {
    if (key === 'CLAUDECODE' || key === 'CLAUDE_PID' || key === 'CLAUDE_EFFORT') continue;
    if (key.startsWith('CLAUDE_CODE_') && !KEEP_CLAUDE_CODE_VARS.test(key)) continue;
    out[key] = value;
  }
  return out;
}

/**
 * The record name for a background command: a short hash of the text, so rerunning the same job
 * overwrites its record instead of accumulating them (issue #700).
 */
export function backgroundRecordName(command) {
  let hash = 0x811c9dc5;
  for (let i = 0; i < command.length; i++) {
    hash = Math.imul(hash ^ command.charCodeAt(i), 0x01000193) >>> 0;
  }
  return `bg-${hash.toString(16).padStart(8, '0')}`;
}

/**
 * SDK options from the environment. Returns warnings instead of logging so stdout stays protocol-only.
 * @param {object} [extras]
 * @param {string} [extras.routerUrl]     local model router (docs/protocol.md §6.1)
 * @param {object} [extras.memoryServer]  in-process shared memory MCP server (§6.2)
 * @param {object} [extras.recallServer]  in-process deja-vu recall MCP server, read-only (issue #495)
 * @param {object} [extras.coordinateServer]  in-process colony-to-colony coordination MCP server (issue #834)
 * @param {object} [extras.githubServer]  in-process host-proxied GitHub write MCP server (issue #778)
 * @param {object} [extras.waitServer]    in-process wait MCP server, built for every colony (issue #181)
 * @param {string[]} [extras.hiddenEnv]   variables Claude Code must not inherit (provider keys)
 * @param {object[]} [extras.routes]     model routes, for provider timeouts and context limits (§6.5)
 * @param {ConditionalInstructions} [extras.instructions]  conditional instruction hooks (issue #473)
 * @param {object} [extras.execPolicy]   the layered exec policy (issue #471); loaded here when absent
 */
export function buildOptions(env = process.env, { routerUrl, memoryServer, recallServer, coordinateServer, findingsServer, loopServer, githubServer, waitServer, hiddenEnv = [], routes = [], instructions, execPolicy } = {}) {
  const warnings = [];
  const claudeEnv = childEnv(env);
  for (const key of hiddenEnv) delete claudeEnv[key];
  if (routerUrl) claudeEnv.ANTHROPIC_BASE_URL = routerUrl;
  for (const [key, value] of Object.entries(routeEnv(routes, env))) claudeEnv[key] ??= value;
  if (env.COLONIZER_SUBAGENT_MODEL) {
    claudeEnv.CLAUDE_CODE_SUBAGENT_MODEL = env.COLONIZER_SUBAGENT_MODEL;
    // Without FORCE, an agent whose definition names a model keeps it: Claude Code's built-in Explore is `inherit`,
    // so it ran on the orchestrator's model and did most of a colony's reading at that price.
    claudeEnv.CLAUDE_CODE_SUBAGENT_MODEL_FORCE = '1';
  }
  if (env.COLONIZER_BACKGROUND_MODEL) claudeEnv.ANTHROPIC_DEFAULT_HAIKU_MODEL = env.COLONIZER_BACKGROUND_MODEL;
  const memory = Boolean(env.COLONIZER_MEMORY_DIR && memoryServer);
  // Both halves of the recall credential: the mothership sets them only when deja is enabled for
  // this colony's org, so a half-set pair is a misconfiguration, not a reason to half-serve it.
  const recall = Boolean(env.COLONIZER_RECALL_URL && env.COLONIZER_RECALL_TOKEN && recallServer);
  // Colony-to-colony coordination (issue #834): its own gateway URL and token, set for every colony
  // whose gateway token exists, so it is not tied to deja the way recall is.
  const coordinate = Boolean(env.COLONIZER_COORD_URL && env.COLONIZER_COORD_TOKEN && coordinateServer);
  const findings = Boolean(env.COLONIZER_FINDINGS === 'true' && findingsServer);
  const loop = Boolean(env.COLONIZER_LOOP === 'true' && loopServer);
  // Only a GitHub-needing loop's colony (loop_github.rs): the mothership sets the flag when it wrote
  // the read-only context, and the tools only ever ask the host for a call on this colony's repo.
  const github = Boolean(env.COLONIZER_GITHUB === 'true' && githubServer);
  // off: the orchestrator works alone. encourage: it is asked to delegate. enforce: it is only allowed
  // to plan, ask and delegate, and a PreToolUse hook refuses the rest.
  // Enforced unless someone chose otherwise: an unset or unrecognised value delegates, and only an
  // explicit 'off' or 'encourage' loosens it.
  const delegate = ['off', 'encourage'].includes(env.COLONIZER_DELEGATE) ? env.COLONIZER_DELEGATE : 'enforce';
  // Plugin directories arrive already mounted read-only in the VM; the mothership
  // rewrites COLONIZER_PLUGIN_DIRS to the in-VM paths. settingSources stays
  // ['project'], so this is the only way a plugin reaches a colony — a user-scope
  // install would be invisible, and a project-scope one would land in the PR.
  const pluginDirs = (env.COLONIZER_PLUGIN_DIRS || '')
    .split(',')
    .map((dir) => dir.trim())
    .filter(Boolean);
  const appended = [SYSTEM_PROMPT_APPEND];
  if (waitServer) appended.push(WAIT_PROMPT_APPEND);
  if (env.COLONIZER_IMAGE) appended.push(environmentPrompt(env.COLONIZER_IMAGE, packageManager(process.cwd())));
  if (memory) appended.push(MEMORY_PROMPT_APPEND);
  if (recall) appended.push(RECALL_PROMPT_APPEND);
  if (coordinate) appended.push(COORDINATION_PROMPT_APPEND);
  if (findings) appended.push(FINDINGS_PROMPT_APPEND);
  if (loop) appended.push(loopPromptAppend(env.COLONIZER_LOOP_SELF_PACED === 'true'));
  if (github) appended.push(GITHUB_PROMPT_APPEND);
  if (delegate !== 'off') appended.push(DELEGATE_PROMPT_APPEND);
  // Only under enforce: encourage has no gate, so a list of allowed tools would be false there.
  if (delegate === 'enforce') appended.push(ENFORCE_PROMPT_APPEND);
  // Keyed on the skill file, not the directory name, so an operator's own copy of superpowers switches on too.
  for (const dir of pluginDirs) {
    const skill = readText(join(dir, SUPERPOWERS_SKILL));
    if (skill !== null) appended.push(superpowersBootstrap(skill.trimEnd()));
  }
  // Token savings (docs/protocol.md): each is off unless its setting is on and the mothership mounted it.
  if (env.COLONIZER_CAVEMAN === 'true') {
    const skill = readText(env.COLONIZER_CAVEMAN_SKILL || CAVEMAN_SKILL);
    const level = CAVEMAN_LEVELS.has(env.COLONIZER_CAVEMAN_LEVEL) ? env.COLONIZER_CAVEMAN_LEVEL : 'full';
    if (skill === null) warnings.push('caveman is switched on but its ruleset is not mounted; replies stay as they are');
    else appended.push(cavemanPrompt(skill, level));
  }
  const rtkBin = env.COLONIZER_RTK === 'true' ? env.COLONIZER_RTK_BIN || RTK_BIN : null;
  // Rewritten commands call `rtk`, so it has to be on the PATH the Bash tool runs with.
  if (rtkBin) claudeEnv.PATH = `${dirname(rtkBin)}:${claudeEnv.PATH ?? '/usr/local/bin:/usr/bin:/bin'}`;
  // Jev compaction is default-off: only 'true' loads the plugin, and childEnv already drops a stale
  // CLAUDE_CODE_ENABLE_FUNCTION_HOOKS from the input env, so the flag below is the only way it is set.
  const jevDir = env.COLONIZER_JEV_COMPACTION === 'true' ? env.COLONIZER_JEV_COMPACTION_DIR || JEV_COMPACTION_DIR : null;
  if (jevDir) claudeEnv.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS = '1';

  const options = {
    cwd: process.cwd(),
    pathToClaudeCodeExecutable: env.COLONIZER_CLAUDE_BIN || '/opt/claude/bin/claude',
    // Not bypassPermissions: AskUserQuestion only reaches canUseTool when nothing auto-approves it.
    permissionMode: 'default',
    includePartialMessages: true,
    // Without this the SDK forwards only a subagent's tool_use/tool_result blocks, so its work
    // appears in the transcript as the orchestrator's own. The colony UI shows each subagent as
    // its own speaker, which needs the text and thinking too.
    forwardSubagentText: true,
    systemPrompt: {
      type: 'preset',
      preset: 'claude_code',
      append: appended.join('\n'),
    },
    settingSources: ['project'],
    env: claudeEnv,
    stderr: (data) => process.stderr.write(data),
  };
  // No allowedTools entry: canUseTool already allows every tool except AskUserQuestion, and listing
  // them would make the SDK warn that canUseTool is shadowed.
  const mcpServers = {};
  if (waitServer) mcpServers[WAIT_SERVER] = waitServer;
  if (memory) mcpServers[MEMORY_SERVER] = memoryServer;
  if (recall) mcpServers[RECALL_SERVER] = recallServer;
  if (coordinate) mcpServers[COORDINATION_SERVER] = coordinateServer;
  if (findings) mcpServers[FINDINGS_SERVER] = findingsServer;
  if (loop) mcpServers[LOOP_SERVER] = loopServer;
  if (github) mcpServers[GITHUB_SERVER] = githubServer;
  if (Object.keys(mcpServers).length) options.mcpServers = mcpServers;
  const preToolUse = [];
  if (delegate === 'enforce') {
    // A hook rather than canUseTool: only the hook input says whether a call came from a subagent
    // (`agent_id`), and without that the gate would refuse the subagents' work as well as the
    // orchestrator's. Denying here reaches the model as a tool error it can act on.
    preToolUse.push({
      hooks: [
        async (input) => {
          const reason = delegationDecision(input.tool_name, input.tool_input, input);
          if (!reason) return { continue: true };
          return {
            continue: true,
            hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: reason },
          };
        },
      ],
    });
  }
  if (loop) {
    // Pacing and stopping the loop stay with the orchestrator, as filing findings does.
    preToolUse.push({
      hooks: [
        async (input) => {
          const reason = loopDecision(input.tool_name, input);
          if (!reason) return { continue: true };
          return {
            continue: true,
            hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: reason },
          };
        },
      ],
    });
  }
  if (findings) {
    // Whatever the delegation mode, filing stays with the orchestrator: a subagent's call carries
    // `agent_id`, and is refused with a reason that tells it to report the finding instead.
    preToolUse.push({
      hooks: [
        async (input) => {
          const reason = findingDecision(input.tool_name, input);
          if (!reason) return { continue: true };
          return {
            continue: true,
            hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: reason },
          };
        },
      ],
    });
  }
  if (github) {
    // GitHub writes stay with the orchestrator too: a subagent's call carries `agent_id`, and is
    // refused with a reason that tells it to report what it found instead.
    preToolUse.push({
      hooks: [
        async (input) => {
          const reason = githubDecision(input.tool_name, input);
          if (!reason) return { continue: true };
          return {
            continue: true,
            hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: reason },
          };
        },
      ],
    });
  }
  if (memory) {
    // The same for shared memory: a subagent searches it but never proposes to it, so every proposal
    // under review is one the orchestrator chose to make.
    preToolUse.push({
      hooks: [
        async (input) => {
          const reason = memoryDecision(input.tool_name, input);
          if (!reason) return { continue: true };
          return {
            continue: true,
            hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: reason },
          };
        },
      ],
    });
  }
  // Exec policy (issue #471): every Bash command meets the layered rules in execpolicy.mjs — a
  // deny is refused with the rule named, an ask surfaces as a colony question through canUseTool.
  // Loaded once here (the repo layer's file is read at start, so the agent rewriting it mid-run
  // cannot widen anything); the same object rides along to runAgent for the ask path.
  const policy = execPolicy ?? loadExecPolicy(env);
  for (const warning of policy.warnings) warnings.push(warning);
  preToolUse.push({
    matcher: 'Bash',
    hooks: [
      async (input) => {
        const command = input.tool_input?.command;
        if (typeof command !== 'string' || !command) return { continue: true };
        const hit = evaluateExecPolicy(policy, command, { cwd: process.cwd() });
        if (!hit) return { continue: true };
        // One harness-log line per decision (agentd turns stderr lines into `log` events).
        process.stderr.write(`${execPolicyLogLine(hit, command)}\n`);
        if (hit.decision === 'allow') return { continue: true };
        return {
          continue: true,
          hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: hit.decision, permissionDecisionReason: execPolicyReason(hit) },
        };
      },
    ],
  });
  if (rtkBin) {
    // In process, like the delegation gate: rtk's own Claude Code hook is a shell script needing jq, and
    // Colonizer doesn't run plugin hooks. Only the input is updated; no permission decision is returned,
    // so this can never allow what the delegation gate denies.
    preToolUse.push({
      matcher: 'Bash',
      hooks: [
        async (input) => {
          const command = input.tool_input?.command;
          if (typeof command !== 'string' || !command) return { continue: true };
          const rewritten = await rtkRewrite(command, rtkBin);
          if (!rewritten) return { continue: true };
          return {
            continue: true,
            hookSpecificOutput: { hookEventName: 'PreToolUse', updatedInput: { ...input.tool_input, command: rewritten } },
          };
        },
      ],
    });
  }
  if (preToolUse.length) options.hooks = { ...options.hooks, PreToolUse: preToolUse };
  // The denial layer's delivery path (denials.mjs): PostToolUseFailure sees the error text of a
  // failed tool call, and additionalContext binds the hint to that very call mid-turn. It returns
  // no decision and touches no tool result, so it can only advise — never grant.
  const denialClassesHinted = new Set(); // one hint per denial class per session
  options.hooks = {
    ...options.hooks,
    PostToolUseFailure: [
      {
        hooks: [
          async (input) => {
            const denial = classifyDenial(input.error);
            if (!denial || denialClassesHinted.has(denial.class)) return { continue: true };
            denialClassesHinted.add(denial.class);
            return {
              continue: true,
              hookSpecificOutput: { hookEventName: 'PostToolUseFailure', additionalContext: denialGuidance([denial.class]) },
            };
          },
        ],
      },
    ],
  };
  if (env.COLONIZER_SERVICES_DIR) {
    // Background Bash calls get a restart:false service record (issue #700), so a resumed boot's
    // relaunch report can name what the suspension killed. The record carries the command text,
    // never an env value, and a failed write is one log line — never a blocked tool call.
    options.hooks = {
      ...options.hooks,
      PostToolUse: [{
        matcher: 'Bash',
        hooks: [async ({ tool_input }) => {
          try {
            const command = tool_input?.command;
            if (tool_input?.run_in_background && typeof command === 'string' && command.trim()) {
              const name = backgroundRecordName(command);
              writeFileSync(join(env.COLONIZER_SERVICES_DIR, `${name}.json`),
                JSON.stringify({ name, cmd: command, restart: false, source: 'background' }));
            }
          } catch (err) {
            process.stderr.write(`colonizer: recording a background command failed: ${err?.message ?? err}\n`);
          }
          return { continue: true };
        }],
      }],
    };
  }
  if (instructions) {
    // Conditional instructions (issue #473): appended after the gates above — they arrive first at
    // index 0 — and never carrying a permission decision, so they cannot allow or deny anything.
    // SessionStart re-injects after a compaction; runAgent's compact_boundary fallback covers the
    // case where the SDK fires no SessionStart for one.
    const withEntry = (entries, entry) => [...(entries ?? []), entry];
    options.hooks = {
      ...options.hooks,
      PreToolUse: withEntry(options.hooks?.PreToolUse, {
        matcher: PATH_TOOLS_MATCHER,
        hooks: [async (input) => instructions.preToolUse(input)],
      }),
      UserPromptSubmit: withEntry(options.hooks?.UserPromptSubmit, {
        hooks: [async (input) => instructions.userPromptSubmit(input)],
      }),
      SessionStart: withEntry(options.hooks?.SessionStart, {
        hooks: [async (input) => instructions.sessionStart(input)],
      }),
    };
  }
  if (pluginDirs.length) {
    options.plugins = pluginDirs.map((path) => ({ type: 'local', path }));
  }
  if (jevDir) {
    // Beside the skill dirs: the superpowers scan above only reads skill files, so this changes
    // nothing about the prompt.
    (options.plugins ??= []).push({ type: 'local', path: jevDir });
    // The hook only accepts real numbers; anything else falls back to its own defaults.
    const jevOptions = {};
    const keepThreshold = Number(env.COLONIZER_JEV_KEEP_THRESHOLD);
    if (Number.isFinite(keepThreshold)) jevOptions.keepThreshold = keepThreshold;
    const preserveRecent = Number(env.COLONIZER_JEV_PRESERVE_RECENT);
    if (Number.isFinite(preserveRecent)) jevOptions.preserveRecentMessages = preserveRecent;
    options.settings = {
      ...options.settings,
      pluginConfigs: { ...options.settings?.pluginConfigs, [JEV_PLUGIN_ID]: { options: jevOptions } },
    };
    // Headless Claude Code keeps the plugin's verdict only in its debug log, which runAgent reads back.
    options.debugFile = env.COLONIZER_JEV_COMPACTION_LOG || join(tmpdir(), 'colonizer-jev-compaction.log');
  }
  if (env.COLONIZER_MODEL) options.model = env.COLONIZER_MODEL;
  // A colony suspended while it waited on its user boots to deliver the answer (issue #562): the
  // session id the agent reported last run continues that conversation, whose transcript the
  // mothership kept on the host. Absent means a fresh conversation.
  if (env.COLONIZER_RESUME_SESSION) options.resume = env.COLONIZER_RESUME_SESSION;
  // Harness-level tool switch (module.json `disabled_tools`): the session's own disallow list, on top
  // of whatever the provider strips per connection. The SDK removes these tools from the model's
  // context entirely, so the setting holds whatever endpoint serves the model.
  const disabledTools = (env.COLONIZER_DISABLED_TOOLS || '')
    .split(',')
    .map((name) => name.trim())
    .filter(Boolean);
  if (disabledTools.length) options.disallowedTools = disabledTools;
  if (env.COLONIZER_EFFORT) {
    if (EFFORT_LEVELS.has(env.COLONIZER_EFFORT)) options.effort = env.COLONIZER_EFFORT;
    else warnings.push(`ignoring COLONIZER_EFFORT=${env.COLONIZER_EFFORT}; expected one of ${[...EFFORT_LEVELS].join(', ')}`);
  }
  // `repo-explorer` is added every time; general-purpose/Explore are only redefined (to carry an
  // effort) when COLONIZER_SUBAGENT_EFFORT is set, so unset still leaves those two built-ins in place.
  if (env.COLONIZER_SUBAGENT_EFFORT && !EFFORT_LEVELS.has(env.COLONIZER_SUBAGENT_EFFORT)) {
    warnings.push(`ignoring COLONIZER_SUBAGENT_EFFORT=${env.COLONIZER_SUBAGENT_EFFORT}; expected one of ${[...EFFORT_LEVELS].join(', ')}`);
  }
  options.agents = subagentDefinitions(EFFORT_LEVELS.has(env.COLONIZER_SUBAGENT_EFFORT) ? env.COLONIZER_SUBAGENT_EFFORT : undefined);
  return { options, warnings };
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Runs one Claude Code session.
 * @param {object} args
 * @param {Function} args.query     the Agent SDK `query` (injected for tests)
 * @param {AsyncIterable<object>} args.commands  parsed stdin commands
 * @param {Function} args.emit      writes one protocol event
 * @param {object} [args.options]   SDK options (canUseTool is added here)
 * @param {object} [args.execPolicy]  the layered exec policy (issue #471); an `ask` becomes a question
 * @param {object} [args.execAllowCache]  the colony's remembered Allows (issue #759); one per run when absent
 * @param {object} [args.pathPolicy]  the mounted path policy (issue #647), as loadPathPolicy returned; a path-taking
 *   tool call that lands on a masked or protected path emits one `path_policy` event per (access, path)
 * @param {number} [args.graceMs]   how long shutdown waits for Claude Code before force-closing
 * @param {boolean} [args.enforceChoices]  re-prompt once when a turn ends with a plain-text question
 * @param {ConditionalInstructions} [args.instructions]  told when a compaction happened (issue #473)
 */
export async function runAgent({ query, commands, emit, options = {}, execPolicy = null, execAllowCache = createExecAllowCache(), pathPolicy = null, graceMs = 8000, enforceChoices = true, instructions = null }) {
  let status = null;
  const setStatus = (state, detail) => {
    if (state === status && detail === undefined) return;
    status = state;
    emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail });
  };

  const input = new AsyncQueue();
  const pending = new Map(); // question_id -> { resolve }
  const askIds = new Set(); // tool_use ids of AskUserQuestion calls
  const subagentAsks = new Set(); // AskUserQuestion tool_use ids seen in a subagent's message
  const toolMessage = new Map(); // tool_use id -> message_id
  const streams = new Map(); // message_id -> Map<block index, { type, id, text, final }>
  const fallbackIndex = new Map(); // message_id -> next index when nothing was streamed
  const subagents = new Map(); // Task tool_use id -> { id, name, description }
  let streamMessageId = null;
  let jevDebugOffset = 0; // bytes of the debug log already scanned for a Jev verdict
  let currentModel = null; // the orchestrator model last announced in a model_changed
  let agentSession = null; // the session id last announced in an agent_session
  // Jev visibility ladder bookkeeping: a decision token indexes tool-use/tool-result pairs by the
  // order their calls were made, so track calls still awaiting a result and the resolved pairs a
  // compaction pass can then remove from (an applied drop_call) or score again.
  const jevPendingCalls = new Map(); // tool_call_id -> { tool, result } until the result arrives
  let jevLivePairs = []; // pairs present in the transcript, in call order: { tool_call_id, tool }
  const pathPolicySeen = new Set(); // `access\0path` already reported, so one attempt is one event

  /**
   * Who produced a message. The SDK sets `parent_tool_use_id` to the Task call that started the
   * subagent, and that call's input named it, so the two together identify the speaker. Returns
   * undefined for the orchestrator's own messages, which carry no agent field at all.
   */
  const agentOf = (parentToolUseId) => {
    if (!parentToolUseId) return undefined;
    return subagents.get(parentToolUseId) ?? { id: parentToolUseId, name: 'subagent', description: null };
  };

  /** `agent` is omitted rather than null for the orchestrator, so its events keep their shape. */
  const withAgent = (event, parentToolUseId) => {
    const agent = agentOf(parentToolUseId);
    return agent ? { ...event, agent } : event;
  };
  let turnActive = false;
  let closing = false;
  let nudged = false; // one choice-card nudge per user message

  const settleStatus = () => {
    if (pending.size > 0) setStatus('waiting_for_answer');
    else setStatus(turnActive ? 'working' : 'idle');
  };

  /** Puts one question to the colony and resolves with its answer (null when cancelled or shut down). */
  const putQuestion = (questionId, questions, { signal, kind = null, blocking = false }) => {
    const reply = new Promise((resolve) => {
      pending.set(questionId, { resolve });
      if (signal?.aborted) resolve(null);
      signal?.addEventListener('abort', () => resolve(null), { once: true });
    });
    emit({
      type: 'question',
      question_id: questionId,
      message_id: toolMessage.get(questionId) ?? null,
      risk: riskClass(questions),
      ...(kind ? { kind } : {}),
      // A tool call is blocked in flight on this answer inside a live agent (issue #759), so the
      // mothership must not suspend the colony: a resumed transcript cannot finish the call.
      ...(blocking ? { blocking: true } : {}),
      questions,
    });
    settleStatus();
    return reply;
  };

  /** Records that a question closed — answered, or not — and settles the status it held. */
  const settleAnswer = (questionId, answer) => {
    pending.delete(questionId);
    if (answer) emit({ type: 'question_answered', question_id: questionId, answers: answer.answers, response: answer.response });
    settleStatus();
  };

  const canUseTool = async (toolName, toolInput, { signal, toolUseID, agentID } = {}) => {
    if (toolName !== ASK_TOOL) {
      // An exec-policy `ask` reaches canUseTool the same way an AskUserQuestion does: the SDK turns
      // the PreToolUse hook's ask decision into a permission request here. The hook has already
      // refused `deny`; this only has to put the question to the colony and map the answer.
      if (execPolicy && toolName === 'Bash' && typeof toolInput?.command === 'string' && toolInput.command) {
        const hit = evaluateExecPolicy(execPolicy, toolInput.command, { cwd: process.cwd() });
        if (hit?.decision === 'deny') return { behavior: 'deny', message: execPolicyReason(hit) };
        if (hit?.decision === 'ask') {
          const allowed = await askColony(hit, toolInput, { signal, toolUseID });
          return allowed
            ? { behavior: 'allow', updatedInput: toolInput }
            : { behavior: 'deny', message: execPolicyReason(hit) };
        }
      }
      return { behavior: 'allow', updatedInput: toolInput };
    }

    const questionId = toolUseID || `question-${askIds.size + 1}`;
    askIds.add(questionId);
    // A subagent's question blocks its Task call in flight (issue #759): suspending the colony
    // would kill the subagent, and a resumed lead transcript would get an answer to a question it
    // never asked. The SDK names the subagent in `agentID`; the tool_use arriving in a subagent's
    // message says the same when it does not. The lead's own question is left unmarked: its turn
    // resumes cleanly with the answer as the next message, so suspending it saves a slot.
    const blocking = Boolean(agentID) || subagentAsks.has(questionId);
    const answer = await putQuestion(questionId, normalizeQuestions(toolInput), { signal, blocking });
    if (!answer) {
      settleAnswer(questionId, null);
      return { behavior: 'deny', message: 'The question was cancelled before the user answered.' };
    }
    settleAnswer(questionId, answer);
    const updatedInput = { ...toolInput, answers: answer.answers };
    if (answer.response) updatedInput.response = answer.response;
    return { behavior: 'allow', updatedInput };
  };

  /**
   * Raises an exec-policy `ask` as a colony question (the same `question`/`question_answered` pair
   * AskUserQuestion uses, so the cockpit card and the autonomy judge both work unchanged) and
   * resolves true only when the answer is Allow. Any other answer — Deny, a free-text "Other",
   * a cancellation — leaves the command refused with the policy reason. The question carries
   * `kind: "exec_policy"` so the mothership keeps the colony running while it waits (issue #759),
   * and an Allow is remembered for the colony: the same command under the same rule, from this
   * agent or a subagent spawned after it, runs without asking again.
   */
  const askColony = async (hit, toolInput, { signal, toolUseID }) => {
    if (execAllowCache.has(hit, toolInput.command)) {
      process.stderr.write(`exec policy: allowed earlier in this colony rule=${hit.rule} layer=${hit.layer}\n`);
      return true;
    }
    const questionId = toolUseID || `exec-policy-${pending.size + 1}`;
    const questions = normalizeQuestions(execPolicyQuestion(hit, toolInput.command));
    const answer = await putQuestion(questionId, questions, { signal, kind: EXEC_POLICY_QUESTION_KIND, blocking: true });
    settleAnswer(questionId, answer);
    const allowed = Boolean(answer) && Object.values(answer.answers ?? {}).some((label) => label === 'Allow');
    if (allowed) execAllowCache.remember(hit, toolInput.command);
    return allowed;
  };

  const blockSlot = (messageId, index) => {
    let blocks = streams.get(messageId);
    if (!blocks) streams.set(messageId, (blocks = new Map()));
    let slot = blocks.get(index);
    if (!slot) blocks.set(index, (slot = { type: null, id: null, text: '', final: false }));
    return slot;
  };

  const onStreamEvent = (event, parent = null) => {
    switch (event?.type) {
      case 'message_start':
        streamMessageId = event.message?.id ?? null;
        break;
      case 'content_block_start': {
        if (!streamMessageId) break;
        const slot = blockSlot(streamMessageId, event.index);
        slot.type = event.content_block?.type ?? null;
        slot.id = event.content_block?.id ?? null;
        if (slot.type === 'tool_use' && slot.id) toolMessage.set(slot.id, streamMessageId);
        break;
      }
      case 'content_block_delta':
        if (streamMessageId && event.delta?.type === 'text_delta' && event.delta.text) {
          const slot = blockSlot(streamMessageId, event.index);
          slot.type ??= 'text';
          slot.text += event.delta.text;
          emit(
            withAgent(
              {
                type: 'assistant_text_delta',
                message_id: streamMessageId,
                block_index: event.index,
                delta: event.delta.text,
              },
              parent,
            ),
          );
        }
        break;
    }
  };

  // Complete assistant messages may carry one block at a time, so recover the streamed block index.
  const blockIndex = (messageId, block) => {
    const blocks = streams.get(messageId);
    if (blocks?.size) {
      const matches = (slot) =>
        block.type === 'tool_use' ? slot.id === block.id : block.type === 'text' ? slot.text === block.text : true;
      for (const [index, slot] of blocks) {
        if (!slot.final && slot.type === block.type && matches(slot)) return ((slot.final = true), index);
      }
      for (const [index, slot] of blocks) {
        if (!slot.final && slot.type === block.type) return ((slot.final = true), index);
      }
    }
    const next = fallbackIndex.get(messageId) ?? (blocks?.size ? Math.max(...blocks.keys()) + 1 : 0);
    fallbackIndex.set(messageId, next + 1);
    return next;
  };

  /**
   * The path policy's runtime report (issue #647): a path-taking tool call that lands on a masked
   * or protected path emits one `path_policy` event per (access, path) per run. Reporting only —
   * no decision, no permission field; the mount enforced before this ever ran (pathpolicy.mjs).
   */
  const reportPathPolicy = (block, parent) => {
    const event = evaluatePathPolicy(pathPolicy, block.name, block.input ?? {}, { workspace: process.cwd() });
    if (!event) return;
    const key = `${event.access}\u0000${event.path}`;
    if (pathPolicySeen.has(key)) return;
    pathPolicySeen.add(key);
    emit(withAgent(event, parent));
  };

  const onAssistant = (msg) => {
    const messageId = msg.message?.id ?? msg.uuid ?? null;
    const parent = msg.parent_tool_use_id ?? null;
    const content = Array.isArray(msg.message?.content) ? msg.message.content : [];
    for (const block of content) {
      const index = blockIndex(messageId, block);
      if (block.type === 'text') {
        if (block.text) {
          emit(withAgent({ type: 'assistant_text', message_id: messageId, block_index: index, text: block.text }, parent));
        }
      } else if (block.type === 'thinking') {
        if (block.thinking?.trim()) {
          emit(withAgent({ type: 'thinking', message_id: messageId, block_index: index, text: block.thinking }, parent));
        }
      } else if (block.type === 'tool_use') {
        toolMessage.set(block.id, messageId);
        // A Task call names the subagent it is about to start; its messages arrive later carrying
        // this call's id as their parent, which is the only thing tying the two together.
        if (block.name === 'Task' || block.name === 'Agent') {
          const input = block.input ?? {};
          subagents.set(block.id, {
            id: block.id,
            name: String(input.subagent_type || input.description || 'subagent'),
            description: input.description ? String(input.description) : null,
          });
        }
        if (block.name === ASK_TOOL) {
          askIds.add(block.id);
          if (parent) subagentAsks.add(block.id);
        } else {
          jevPendingCalls.set(block.id, { tool: block.name, result: false });
          emit(
            withAgent(
              {
                type: 'tool_call',
                message_id: messageId,
                tool_call_id: block.id,
                name: block.name,
                input: block.input ?? {},
              },
              parent,
            ),
          );
          reportPathPolicy(block, parent);
        }
      }
    }
  };

  const onUser = (msg) => {
    const content = msg.message?.content;
    if (!Array.isArray(content)) return;
    const parent = msg.parent_tool_use_id ?? null;
    for (const block of content) {
      if (block?.type !== 'tool_result' || askIds.has(block.tool_use_id)) continue;
      const output = toolResultText(block.content);
      // A subagent started in the background answers its Task call with an immediate launch ack,
      // not a report — the SDK marks it `async_launched`, and the real result arrives later as a
      // task_notification (below). Mark the ack so a resumed boot does not read the subagent as
      // finished the moment it started (issue #756).
      const background = msg.tool_use_result?.status === 'async_launched';
      // The denial layer only ever adds a `denial` field to errored results; is_error and the
      // output are exactly as they would be without it (denials.mjs).
      const event = annotateDenial(
        withAgent(
          {
            type: 'tool_result',
            tool_call_id: block.tool_use_id,
            output,
            is_error: Boolean(block.is_error),
            ...(background ? { background: true } : {}),
          },
          parent,
        ),
        output,
      );
      emit(event);
      const pendingCall = jevPendingCalls.get(block.tool_use_id);
      if (pendingCall) {
        pendingCall.result = true;
        // Results can arrive out of call order; completing one moves every resolved call, so the
        // live pairs stay in the order their calls were made.
        for (const [id, call] of jevPendingCalls) {
          if (!call.result) continue;
          jevLivePairs.push({ tool_call_id: id, tool: call.tool });
          jevPendingCalls.delete(id);
        }
      }
    }
  };

  const onResult = (msg) => {
    turnActive = false;
    streams.clear();
    fallbackIndex.clear();
    streamMessageId = null;
    const result = typeof msg.result === 'string' ? msg.result : null;
    if (enforceChoices && !closing && !nudged && !msg.is_error && pending.size === 0 && endsWithQuestion(result)) {
      // Withhold turn_end (so autopilot can't publish mid-question) and have the agent re-ask with choices.
      nudged = true;
      emit({ type: 'log', level: 'info', message: 'The agent asked in plain text; asking it to use a choice card instead.' });
      input.push({ type: 'user', message: { role: 'user', content: CHOICE_NUDGE }, parent_tool_use_id: null, isSynthetic: true });
      turnActive = true;
      settleStatus();
      return;
    }
    const usage = summariseUsage(msg.modelUsage);
    emit({
      type: 'turn_end',
      is_error: Boolean(msg.is_error),
      result,
      // Claude models only when the SDK says which model cost what; the SDK's total prices routed models as Claude.
      cost_usd: usage ? usage.claudeCostUsd : typeof msg.total_cost_usd === 'number' ? msg.total_cost_usd : null,
      duration_ms: typeof msg.duration_ms === 'number' ? msg.duration_ms : null,
      ...(usage ? { model_usage: usage.models } : {}),
    });
    settleStatus();
  };

  setStatus('idle');
  const q = query({ prompt: input, options: { ...options, canUseTool } });

  const consume = (async () => {
    try {
      for await (const msg of q) {
        switch (msg?.type) {
          case 'stream_event':
            turnActive = true;
            settleStatus();
            onStreamEvent(msg.event, msg.parent_tool_use_id ?? null);
            break;
          case 'assistant':
            turnActive = true;
            settleStatus();
            onAssistant(msg);
            break;
          case 'user':
            onUser(msg);
            break;
          case 'result':
            onResult(msg);
            break;
          case 'system':
            // Conditional instructions (issue #473): the main window was just summarised. A
            // SessionStart(compact) hook usually rebuilds the injected set itself; this covers one
            // where the hook never fired.
            if (msg.subtype === 'compact_boundary') instructions?.markCompacted();
            if (msg.subtype === 'init') {
              // The id a resumed boot continues (issue #562); the mothership keeps it on the
              // colony's record. Announced only when it is news, like the model below.
              if (typeof msg.session_id === 'string' && msg.session_id && msg.session_id !== agentSession) {
                agentSession = msg.session_id;
                emit({ type: 'agent_session', session_id: msg.session_id });
              }
              emit({ type: 'log', level: 'info', message: `Claude Code session ${msg.session_id} started (model ${msg.model})` });
              if (jevEnabled(options)) {
                // A restarted runAgent must not re-report a verdict from before it started.
                try { jevDebugOffset = statSync(options.debugFile).size; } catch { jevDebugOffset = 0; }
                if (!jevVersionOk(msg.claude_code_version)) {
                  emit({ type: 'log', level: 'warn', message: `Jev compaction needs Claude Code 2.1.274 or later (function hooks), but this colony runs ${msg.claude_code_version}; compaction falls back to the built-in summary` });
                } else if (!msg.plugins?.some((p) => p?.name === JEV_PLUGIN)) {
                  emit({ type: 'log', level: 'warn', message: "Jev compaction is switched on, but Claude Code didn't load the fast-jev-compaction plugin; compaction falls back to the built-in summary" });
                }
              }
              // init can come once per turn: announce the model only when it is news to clients.
              if (typeof msg.model === 'string' && msg.model && msg.model !== currentModel) {
                emit({ type: 'model_changed', model: msg.model, previous: currentModel });
                currentModel = msg.model;
              }
            } else if (msg.subtype === 'compact_boundary' && jevEnabled(options)) {
              let added = '';
              let fd;
              try {
                fd = openSync(options.debugFile, 'r');
                const size = fstatSync(fd).size;
                if (size < jevDebugOffset) jevDebugOffset = 0; // rotated or truncated
                const buf = Buffer.alloc(size - jevDebugOffset);
                readSync(fd, buf, 0, buf.length, jevDebugOffset);
                added = buf.toString('utf8');
                jevDebugOffset = size;
              } catch {
                // A missing or unreadable debug log means no verdict, never an error.
              } finally {
                if (fd !== undefined) try { closeSync(fd); } catch {}
              }
              const meta = msg.compact_metadata ?? {};
              const verdict = latestJevVerdict(added);
              emit({ type: 'log', level: 'info', message: `Compaction (${meta.trigger}, ${meta.pre_tokens} → ${meta.post_tokens ?? '?'} tokens): ${verdict ?? 'fast-jev-compaction left no verdict'}` });
              // The visibility ladder's measurement (issue #475): map the decisions back onto the
              // pairs they were about, so later runs can score what Jev kept against what the agent
              // actually needed. A fallback pass applied nothing, so its decisions are shadow data
              // and no pair leaves the list.
              const decisions = parseJevDecisions(added);
              if (decisions.length) {
                // No verdict line at all is as unconfirmed as a fallback one: only a verdict that
                // positively isn't the fallback may drop pairs from the live list.
                const applied = verdict !== null && !/^fallback to built-in summary/i.test(verdict);
                const pairs = jevLivePairs.filter((pair) => pair.tool_call_id && pair.tool);
                const ladder = [];
                for (const d of decisions) {
                  const pair = pairs[d.n - 1];
                  // Pinned pairs never get a token, so n has gaps; an out-of-range n (shouldn't
                  // happen) names no pair we track and is skipped.
                  if (pair) ladder.push({ tool_call_id: pair.tool_call_id, tool: pair.tool, action: d.action, keep_call: d.keepCall, keep_result: d.keepResult });
                }
                emit({ type: 'jev_ladder', applied, pre_tokens: meta.pre_tokens, post_tokens: meta.post_tokens, trigger: meta.trigger, decisions: ladder });
                if (applied) {
                  const dropped = new Set(ladder.filter((d) => d.action === 'drop_call').map((d) => d.tool_call_id));
                  jevLivePairs = jevLivePairs.filter((pair) => !dropped.has(pair.tool_call_id));
                }
              }
            } else if (msg.subtype === 'task_notification' && msg.tool_use_id) {
              // A background subagent settled (issue #756): recorded by the Task call that started
              // it, so a resumed boot can tell one that finished from one a suspension left in
              // flight. A `stopped` status is not a completion — like the pinned runner's "stopped
              // by the user", the resume brief exists to report it.
              emit({ type: 'subagent_end', tool_call_id: msg.tool_use_id, status: msg.status });
            }
            break;
        }
      }
    } catch (err) {
      if (!closing) {
        const message = String(err?.message ?? err);
        emit({ type: 'log', level: 'error', message });
        setStatus('error', message);
      }
    }
  })();

  let messageCount = 0;
  commandLoop: for await (const command of commands) {
    switch (command?.type) {
      case 'user_message': {
        const text = typeof command.text === 'string' ? command.text : '';
        if (!text.trim()) {
          emit({ type: 'log', level: 'warn', message: 'ignored an empty user_message' });
          break;
        }
        const id = typeof command.id === 'string' && command.id ? command.id : `u-${++messageCount}`;
        emit({ type: 'user_message', id, text });
        nudged = false;
        input.push({ type: 'user', message: { role: 'user', content: text }, parent_tool_use_id: null });
        turnActive = true;
        settleStatus();
        break;
      }
      case 'answer': {
        const entry = pending.get(command.question_id);
        if (!entry) {
          emit({ type: 'log', level: 'warn', message: `no open question with id ${command.question_id}` });
          break;
        }
        const answers =
          command.answers && typeof command.answers === 'object' && !Array.isArray(command.answers) ? command.answers : {};
        const response = typeof command.response === 'string' && command.response.trim() ? command.response : null;
        entry.resolve({ answers, response });
        break;
      }
      case 'interrupt':
        Promise.resolve()
          .then(() => q.interrupt?.())
          .catch((err) => emit({ type: 'log', level: 'warn', message: `interrupt failed: ${err?.message ?? err}` }));
        break;
      case 'set_model': {
        // Same session and conversation; the SDK applies it from the next response on.
        const model = typeof command.model === 'string' ? command.model.trim() : '';
        if (!model) {
          emit({ type: 'log', level: 'warn', message: 'ignored a set_model without a model' });
          break;
        }
        Promise.resolve()
          .then(() => q.setModel(model))
          .then(() => {
            emit({ type: 'model_changed', model, previous: currentModel });
            currentModel = model;
          })
          .catch((err) => emit({ type: 'log', level: 'warn', message: `set_model ${model} failed: ${err?.message ?? err}` }));
        break;
      }
      case 'shutdown':
        break commandLoop;
      default:
        break; // unknown commands are ignored (protocol forward compatibility)
    }
  }

  // Shutdown or stdin EOF.
  closing = true;
  for (const entry of pending.values()) entry.resolve(null);
  if (turnActive) await Promise.race([Promise.resolve(q.interrupt?.()).catch(() => {}), sleep(2000)]);
  input.close();
  const finished = await Promise.race([consume.then(() => true), sleep(graceMs).then(() => false)]);
  if (!finished) {
    try {
      q.close?.();
    } catch {
      // already closed
    }
    await Promise.race([consume, sleep(2000)]);
  }
  setStatus('exited');
}

async function main() {
  const emit = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
  const commands = new AsyncQueue();

  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  lines.on('line', (line) => {
    if (!line.trim()) return;
    try {
      commands.push(JSON.parse(line));
    } catch {
      emit({ type: 'log', level: 'warn', message: 'ignored a command line that is not valid JSON' });
    }
  });
  lines.on('close', () => commands.close());
  for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => commands.push({ type: 'shutdown' }));

  const { query, createSdkMcpServer, tool } = await import('@anthropic-ai/claude-agent-sdk');

  const plan = routingPlan(process.env);
  for (const message of plan.warnings) emit({ type: 'log', level: 'warn', message });
  let router = null;
  if (plan.needsRouter) {
    router = await startRouter({ routes: plan.routes, env: process.env, log: ({ level, message }) => emit({ type: 'log', level, message }) });
    const served = plan.routes.map((route) => route.prefix).join(', ') || 'none';
    emit({ type: 'log', level: 'info', message: `model router listening on ${router.url} (provider routes: ${served})` });
  }

  // Headroom sits in front of whatever Claude Code would otherwise talk to (docs/protocol.md, "Token savings").
  let headroom = null;
  if (process.env.COLONIZER_HEADROOM === 'true') {
    headroom = await startHeadroom({ env: process.env, upstream: router?.url, log: ({ level, message }) => emit({ type: 'log', level, message }) });
  }
  const { z } = await import('zod');

  let memoryServer;
  if (process.env.COLONIZER_MEMORY_DIR) {
    memoryServer = createMemoryServer({ dir: process.env.COLONIZER_MEMORY_DIR, emit, createSdkMcpServer, tool, z });
  }

  let findingsServer;
  if (process.env.COLONIZER_FINDINGS === 'true') {
    findingsServer = createFindingsServer({ emit, createSdkMcpServer, tool, z });
  }

  // Read-only search of the mothership's deja-vu index: no emit, because nothing leaves the colony.
  let recallServer;
  if (process.env.COLONIZER_RECALL_URL && process.env.COLONIZER_RECALL_TOKEN) {
    recallServer = createRecallServer({ url: process.env.COLONIZER_RECALL_URL, token: process.env.COLONIZER_RECALL_TOKEN, createSdkMcpServer, tool, z });
  }

  // Colony-to-colony coordination (issue #834): its own gateway URL and token, present whenever the
  // colony has a gateway token, whether or not recall is.
  let coordinateServer;
  if (process.env.COLONIZER_COORD_URL && process.env.COLONIZER_COORD_TOKEN) {
    coordinateServer = createCoordinationServer({ url: process.env.COLONIZER_COORD_URL, token: process.env.COLONIZER_COORD_TOKEN, createSdkMcpServer, tool, z });
  }

  // Host-proxied GitHub writes (issue #778): only for a loop that asked for GitHub, which is
  // exactly when the mothership set COLONIZER_GITHUB and wrote /colonizer/github.
  let githubServer;
  if (process.env.COLONIZER_GITHUB === 'true') {
    githubServer = createGithubServer({ emit, createSdkMcpServer, tool, z });
  }

  let loopServer;
  if (process.env.COLONIZER_LOOP === 'true') {
    loopServer = createLoopServer({ emit, createSdkMcpServer, tool, z, selfPaced: process.env.COLONIZER_LOOP_SELF_PACED === 'true' });
  }

  // Every colony can wait: a blocking wait costs no model turn, and the tool has no dependency or
  // setting to gate it on.
  const waitServer = createWaitServer({ createSdkMcpServer, tool, z });

  // Conditional instructions (issue #473): per-directory FOOTGUNS.md and .colonizer/instructions.toml
  // fragments, injected through hooks once per condition per context window and re-injected after a
  // compaction. The task's labels come from the mothership (boot.rs) for label-conditioned rules.
  const instructions = new ConditionalInstructions({
    workspace: process.cwd(),
    labels: parseLabels(process.env.COLONIZER_TASK_LABELS),
    log: ({ level, message }) => emit({ type: 'log', level, message }),
  });

  // The layered exec policy (issue #471), loaded once: the repo layer's file is read before the
  // agent can run anything, and the same object is what runAgent answers questions from.
  const execPolicy = loadExecPolicy(process.env);
  // The mounted path policy (issue #647), loaded once: the bind list the guest booted with is what
  // the runtime reports against. Absent (an older harness) means the feature is off, silently.
  const pathPolicy = loadPathPolicy(process.env);
  const { options, warnings } = buildOptions(process.env, {
    // Claude Code's base URL: Headroom when it is running, which forwards to the router or to Anthropic.
    routerUrl: headroom?.url ?? router?.url,
    memoryServer,
    recallServer,
    coordinateServer,
    findingsServer,
    loopServer,
    githubServer,
    waitServer,
    hiddenEnv: plan.routes.map((route) => route.key_env).filter(Boolean),
    routes: plan.routes,
    instructions,
    execPolicy,
  });
  for (const message of warnings) emit({ type: 'log', level: 'warn', message });
  for (const message of pathPolicy.warnings) emit({ type: 'log', level: 'warn', message });

  // Before the agent sees the workspace, not after. In block mode a finding
  // ends the colony here, with the terminal still reachable for a human.
  const scan = await runPreflight({ env: process.env, emit });
  if (shouldBlock(scan)) {
    emit({ type: 'status', state: 'error', detail: 'pre-flight scan blocked this colony' });
    await headroom?.close();
    await router?.close();
    process.exit(0);
  }

  const enforceChoices = !['0', 'false', 'no', 'off'].includes(String(process.env.COLONIZER_ENFORCE_CHOICES ?? '').toLowerCase());
  await runAgent({ query, commands, emit, options, execPolicy, pathPolicy: pathPolicy.policy, enforceChoices, instructions });
  await headroom?.close();
  await router?.close();
  process.exit(0);
}

const isEntrypoint = (() => {
  try {
    return realpathSync(process.argv[1]) === fileURLToPath(import.meta.url);
  } catch {
    return false;
  }
})();

if (isEntrypoint) {
  main().catch((err) => {
    process.stderr.write(`${err?.stack ?? err}\n`);
    process.stdout.write(`${JSON.stringify({ type: 'status', state: 'exited', detail: String(err?.message ?? err) })}\n`);
    process.exit(1);
  });
}
