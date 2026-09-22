#!/usr/bin/env node
// Colonizer agent runner for Claude Code. Implements the runner contract in docs/protocol.md §2:
// commands arrive as JSON lines on stdin, protocol events leave as JSON lines on stdout.
// Diagnostics go to stderr only.

import { execFile } from 'node:child_process';
import { readFileSync, realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

import { createFindingsServer, FINDINGS_PROMPT_APPEND, FINDINGS_SERVER, findingDecision } from './findings.mjs';
import { createMemoryServer, MEMORY_PROMPT_APPEND, MEMORY_SERVER } from './memory.mjs';
import { createWaitServer, WAIT_PROMPT_APPEND, WAIT_SERVER } from './wait.mjs';
import { startHeadroom } from './headroom.mjs';
import { runPreflight, shouldBlock } from './preflight.mjs';
import { routeEnv, routingPlan, startRouter } from './router.mjs';

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

/**
 * What the colony can and cannot reach, so no model spends a turn discovering it. `image` is the container image the
 * mothership booted (COLONIZER_IMAGE).
 */
export function environmentPrompt(image) {
  return [
    '- This colony has no GitHub access: there is no gh CLI and no GitHub credentials, so the GitHub API and private repositories are out of reach. The issue is already in your brief, and the harness publishes the pull request.',
    `- The colony runs the container image \`${image}\`. Toolchains it does not include (a Rust or Swift toolchain in a Node image, for example) are not installed. Check once with \`command -v\` before relying on one. Install a toolchain only when the task genuinely needs it to build or test; otherwise say in your report what could not be run.`,
  ].join('\n');
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
const ORCHESTRATOR_TOOLS = new Set([
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
 * SDK options from the environment. Returns warnings instead of logging so stdout stays protocol-only.
 * @param {object} [extras]
 * @param {string} [extras.routerUrl]     local model router (docs/protocol.md §6.1)
 * @param {object} [extras.memoryServer]  in-process shared memory MCP server (§6.2)
 * @param {object} [extras.waitServer]    in-process wait MCP server, built for every colony (issue #181)
 * @param {string[]} [extras.hiddenEnv]   variables Claude Code must not inherit (provider keys)
 * @param {object[]} [extras.routes]     model routes, for provider timeouts and context limits (§6.5)
 */
export function buildOptions(env = process.env, { routerUrl, memoryServer, findingsServer, waitServer, hiddenEnv = [], routes = [] } = {}) {
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
  const findings = Boolean(env.COLONIZER_FINDINGS === 'true' && findingsServer);
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
  if (env.COLONIZER_IMAGE) appended.push(environmentPrompt(env.COLONIZER_IMAGE));
  if (memory) appended.push(MEMORY_PROMPT_APPEND);
  if (findings) appended.push(FINDINGS_PROMPT_APPEND);
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
  if (findings) mcpServers[FINDINGS_SERVER] = findingsServer;
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
  if (preToolUse.length) options.hooks = { PreToolUse: preToolUse };
  if (pluginDirs.length) {
    options.plugins = pluginDirs.map((path) => ({ type: 'local', path }));
  }
  if (env.COLONIZER_MODEL) options.model = env.COLONIZER_MODEL;
  if (env.COLONIZER_EFFORT) {
    if (EFFORT_LEVELS.has(env.COLONIZER_EFFORT)) options.effort = env.COLONIZER_EFFORT;
    else warnings.push(`ignoring COLONIZER_EFFORT=${env.COLONIZER_EFFORT}; expected one of ${[...EFFORT_LEVELS].join(', ')}`);
  }
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
 * @param {number} [args.graceMs]   how long shutdown waits for Claude Code before force-closing
 * @param {boolean} [args.enforceChoices]  re-prompt once when a turn ends with a plain-text question
 */
export async function runAgent({ query, commands, emit, options = {}, graceMs = 8000, enforceChoices = true }) {
  let status = null;
  const setStatus = (state, detail) => {
    if (state === status && detail === undefined) return;
    status = state;
    emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail });
  };

  const input = new AsyncQueue();
  const pending = new Map(); // question_id -> { resolve }
  const askIds = new Set(); // tool_use ids of AskUserQuestion calls
  const toolMessage = new Map(); // tool_use id -> message_id
  const streams = new Map(); // message_id -> Map<block index, { type, id, text, final }>
  const fallbackIndex = new Map(); // message_id -> next index when nothing was streamed
  const subagents = new Map(); // Task tool_use id -> { id, name, description }
  let streamMessageId = null;

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

  const canUseTool = async (toolName, toolInput, { signal, toolUseID } = {}) => {
    if (toolName !== ASK_TOOL) return { behavior: 'allow', updatedInput: toolInput };

    const questionId = toolUseID || `question-${askIds.size + 1}`;
    askIds.add(questionId);
    const reply = new Promise((resolve) => {
      pending.set(questionId, { resolve });
      if (signal?.aborted) resolve(null);
      signal?.addEventListener('abort', () => resolve(null), { once: true });
    });
    emit({
      type: 'question',
      question_id: questionId,
      message_id: toolMessage.get(questionId) ?? null,
      questions: normalizeQuestions(toolInput),
    });
    settleStatus();

    const answer = await reply;
    pending.delete(questionId);
    if (!answer) {
      settleStatus();
      return { behavior: 'deny', message: 'The question was cancelled before the user answered.' };
    }
    emit({ type: 'question_answered', question_id: questionId, answers: answer.answers, response: answer.response });
    settleStatus();
    const updatedInput = { ...toolInput, answers: answer.answers };
    if (answer.response) updatedInput.response = answer.response;
    return { behavior: 'allow', updatedInput };
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
        if (block.name === ASK_TOOL) askIds.add(block.id);
        else {
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
      emit(
        withAgent(
          {
            type: 'tool_result',
            tool_call_id: block.tool_use_id,
            output: toolResultText(block.content),
            is_error: Boolean(block.is_error),
          },
          parent,
        ),
      );
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
            if (msg.subtype === 'init') {
              emit({ type: 'log', level: 'info', message: `Claude Code session ${msg.session_id} started (model ${msg.model})` });
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

  // Every colony can wait: a blocking wait costs no model turn, and the tool has no dependency or
  // setting to gate it on.
  const waitServer = createWaitServer({ createSdkMcpServer, tool, z });

  const { options, warnings } = buildOptions(process.env, {
    // Claude Code's base URL: Headroom when it is running, which forwards to the router or to Anthropic.
    routerUrl: headroom?.url ?? router?.url,
    memoryServer,
    findingsServer,
    waitServer,
    hiddenEnv: plan.routes.map((route) => route.key_env).filter(Boolean),
    routes: plan.routes,
  });
  for (const message of warnings) emit({ type: 'log', level: 'warn', message });

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
  await runAgent({ query, commands, emit, options, enforceChoices });
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
