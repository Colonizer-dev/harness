#!/usr/bin/env node
// Colonizer agent runner for Claude Code. Implements the runner contract in docs/protocol.md §2:
// commands arrive as JSON lines on stdin, protocol events leave as JSON lines on stdout.
// Diagnostics go to stderr only.

import { realpathSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

import { createMemoryServer, MEMORY_PROMPT_APPEND, MEMORY_SERVER } from './memory.mjs';
import { routingPlan, startRouter } from './router.mjs';

export const SYSTEM_PROMPT_APPEND = [
  'You are running inside the Colonizer; the user follows along in a web UI.',
  '- Whenever you need a decision, a clarification or any other input from the user, call the AskUserQuestion tool with 2-4 concrete options. Never ask the user in plain text, and never end a turn with a plain-text question. The UI always adds a free-text "Other" choice, so do not add one yourself.',
  '- Do not run `git commit` or `git push` and do not create branches; the harness commits your changes and opens the pull request.',
].join('\n');

export const CHOICE_NUDGE =
  'You ended your turn with a question in plain text. Ask it again with the AskUserQuestion tool, offering 2-4 concrete options, and wait for the answer.';

/** True when a turn's final text ends by asking the user something. */
export function endsWithQuestion(text) {
  if (typeof text !== 'string') return false;
  return /\?[\s*_`'")\]]*$/.test(text.trim());
}

export const MAX_TOOL_OUTPUT = 20_000;
const ASK_TOOL = 'AskUserQuestion';
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
 * @param {string[]} [extras.hiddenEnv]   variables Claude Code must not inherit (provider keys)
 */
export function buildOptions(env = process.env, { routerUrl, memoryServer, hiddenEnv = [] } = {}) {
  const warnings = [];
  const claudeEnv = childEnv(env);
  for (const key of hiddenEnv) delete claudeEnv[key];
  if (routerUrl) claudeEnv.ANTHROPIC_BASE_URL = routerUrl;
  if (env.COLONIZER_SUBAGENT_MODEL) claudeEnv.CLAUDE_CODE_SUBAGENT_MODEL = env.COLONIZER_SUBAGENT_MODEL;
  if (env.COLONIZER_BACKGROUND_MODEL) claudeEnv.ANTHROPIC_DEFAULT_HAIKU_MODEL = env.COLONIZER_BACKGROUND_MODEL;
  const memory = Boolean(env.COLONIZER_MEMORY_DIR && memoryServer);

  const options = {
    cwd: process.cwd(),
    pathToClaudeCodeExecutable: env.COLONIZER_CLAUDE_BIN || '/opt/claude/bin/claude',
    // Not bypassPermissions: AskUserQuestion only reaches canUseTool when nothing auto-approves it.
    permissionMode: 'default',
    includePartialMessages: true,
    systemPrompt: {
      type: 'preset',
      preset: 'claude_code',
      append: memory ? `${SYSTEM_PROMPT_APPEND}\n${MEMORY_PROMPT_APPEND}` : SYSTEM_PROMPT_APPEND,
    },
    settingSources: ['project'],
    env: claudeEnv,
    stderr: (data) => process.stderr.write(data),
  };
  if (memory) {
    // No allowedTools entry: canUseTool already allows every tool except AskUserQuestion, and listing
    // them would make the SDK warn that canUseTool is shadowed.
    options.mcpServers = { [MEMORY_SERVER]: memoryServer };
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
  let streamMessageId = null;
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

  const onStreamEvent = (event) => {
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
          emit({
            type: 'assistant_text_delta',
            message_id: streamMessageId,
            block_index: event.index,
            delta: event.delta.text,
          });
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
    const content = Array.isArray(msg.message?.content) ? msg.message.content : [];
    for (const block of content) {
      const index = blockIndex(messageId, block);
      if (block.type === 'text') {
        if (block.text) emit({ type: 'assistant_text', message_id: messageId, block_index: index, text: block.text });
      } else if (block.type === 'thinking') {
        if (block.thinking?.trim()) {
          emit({ type: 'thinking', message_id: messageId, block_index: index, text: block.thinking });
        }
      } else if (block.type === 'tool_use') {
        toolMessage.set(block.id, messageId);
        if (block.name === ASK_TOOL) askIds.add(block.id);
        else {
          emit({
            type: 'tool_call',
            message_id: messageId,
            tool_call_id: block.id,
            name: block.name,
            input: block.input ?? {},
          });
        }
      }
    }
  };

  const onUser = (msg) => {
    const content = msg.message?.content;
    if (!Array.isArray(content)) return;
    for (const block of content) {
      if (block?.type !== 'tool_result' || askIds.has(block.tool_use_id)) continue;
      emit({
        type: 'tool_result',
        tool_call_id: block.tool_use_id,
        output: toolResultText(block.content),
        is_error: Boolean(block.is_error),
      });
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
    emit({
      type: 'turn_end',
      is_error: Boolean(msg.is_error),
      result,
      cost_usd: typeof msg.total_cost_usd === 'number' ? msg.total_cost_usd : null,
      duration_ms: typeof msg.duration_ms === 'number' ? msg.duration_ms : null,
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
            onStreamEvent(msg.event);
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
    router = await startRouter({ routes: plan.routes, env: process.env });
    const served = plan.routes.map((route) => route.prefix).join(', ') || 'none';
    emit({ type: 'log', level: 'info', message: `model router listening on ${router.url} (provider routes: ${served})` });
  }

  let memoryServer;
  if (process.env.COLONIZER_MEMORY_DIR) {
    const { z } = await import('zod');
    memoryServer = createMemoryServer({ dir: process.env.COLONIZER_MEMORY_DIR, emit, createSdkMcpServer, tool, z });
  }

  const { options, warnings } = buildOptions(process.env, {
    routerUrl: router?.url,
    memoryServer,
    hiddenEnv: plan.routes.map((route) => route.key_env).filter(Boolean),
  });
  for (const message of warnings) emit({ type: 'log', level: 'warn', message });

  const enforceChoices = !['0', 'false', 'no', 'off'].includes(String(process.env.COLONIZER_ENFORCE_CHOICES ?? '').toLowerCase());
  await runAgent({ query, commands, emit, options, enforceChoices });
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
