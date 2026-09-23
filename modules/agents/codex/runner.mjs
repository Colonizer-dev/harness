#!/usr/bin/env node
// Colonizer agent runner for Codex: stdin commands → Codex SDK → stdout protocol events (§2).
// Diagnostics go to stderr only.

import { accessSync, constants, realpathSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

// Codex exec has no ask-the-user tool and no system-prompt channel: these prefix the first turn,
// and questions come back as a fenced block in the final message.
export const SYSTEM_PROMPT_APPEND = [
  'You are running inside the Colonizer; the user follows along in a web UI.',
  '- Headless Codex cannot ask questions mid-turn. Whenever you need a decision, a clarification or any other input from the user, END your reply with a fenced block ```colonizer-question containing JSON {"questions":[{"question","header","multi_select","options":[{"label","description"}]}]} with 2-4 concrete options. Never ask the user in plain text. The UI always adds a free-text "Other" choice, so do not add one yourself.',
  '- Do not run `git commit` or `git push` and do not create branches; the harness commits your changes and opens the pull request.',
].join('\n');
export const QUESTION_FENCE = 'colonizer-question';
export const MAX_TOOL_OUTPUT = 20_000;
const EFFORT_LEVELS = new Set(['minimal', 'low', 'medium', 'high', 'xhigh']);
// Same triple map as the SDK's findCodexPath: the binary ships in an optional platform package.
const TRIPLE = { linux: { x64: 'x86_64-unknown-linux-musl', arm64: 'aarch64-unknown-linux-musl' } };
const PLATFORM_PACKAGE = { 'x86_64-unknown-linux-musl': '@openai/codex-linux-x64', 'aarch64-unknown-linux-musl': '@openai/codex-linux-arm64' };

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
    };
  }
}

export function splitModel(raw) {
  const value = String(raw ?? '').trim();
  const slash = value.indexOf('/');
  return slash < 0 ? { provider: null, model: value } : { provider: value.slice(0, slash), model: value.slice(slash + 1) };
}
export function modelError(raw) {
  const { provider, model } = splitModel(raw);
  // The gateway speaks Anthropic/OpenAI chat, not the Responses API Codex needs (docs/decisions.md).
  if (model && provider && provider !== 'openai') return `COLONIZER_MODEL=${raw.trim()} names provider '${provider}': only openai models (or a bare model name) can run on Codex`;
  return null;
}
export function codexModel(raw) {
  return splitModel(raw).model;
}
export function resolveCodexBin(env = process.env, { platform = process.platform, arch = process.arch } = {}) {
  if (env.COLONIZER_CODEX_BIN) {
    try {
      accessSync(env.COLONIZER_CODEX_BIN, constants.X_OK);
      return { path: env.COLONIZER_CODEX_BIN };
    } catch {
      return { error: `COLONIZER_CODEX_BIN=${env.COLONIZER_CODEX_BIN} is not executable` };
    }
  }
  const triple = TRIPLE[platform]?.[arch];
  const platformPackage = triple ? PLATFORM_PACKAGE[triple] : null;
  if (!platformPackage) return { error: `Codex ships no binary for ${platform}/${arch}` };
  try {
    const codexRequire = createRequire(createRequire(import.meta.url).resolve('@openai/codex/package.json'));
    const vendor = `${codexRequire.resolve(`${platformPackage}/package.json`).replace(/\/package\.json$/, '')}/vendor`;
    for (const candidate of [`${vendor}/${triple}/bin/codex`, `${vendor}/${triple}/codex/codex`]) {
      try {
        if (statSync(candidate).isFile()) {
          accessSync(candidate, constants.X_OK);
          return { path: candidate };
        }
      } catch { /* next candidate */ }
    }
  } catch { /* error below */ }
  return { error: 'the codex binary is missing: install @openai/codex with its optional platform dependencies' };
}
export function preflight(env = process.env, opts) {
  if (!env.OPENAI_API_KEY?.trim() && !env.CODEX_API_KEY?.trim()) return { ok: false, error: 'no API key: set OPENAI_API_KEY or CODEX_API_KEY for api.openai.com' };
  const bin = resolveCodexBin(env, opts);
  if (bin.error) return { ok: false, error: bin.error };
  const invalid = env.COLONIZER_MODEL?.trim() ? modelError(env.COLONIZER_MODEL) : null;
  if (invalid) return { ok: false, error: invalid };
  return { ok: true, bin: bin.path };
}
export function buildThreadOptions(env = process.env, model = '') {
  const options = { sandboxMode: 'danger-full-access', approvalPolicy: 'never', skipGitRepoCheck: true, workingDirectory: process.cwd() };
  if (model) options.model = model;
  const effort = String(env.COLONIZER_EFFORT ?? '').trim();
  if (effort) {
    if (EFFORT_LEVELS.has(effort)) options.modelReasoningEffort = effort;
    else options.warning = `ignoring COLONIZER_EFFORT=${effort}; expected one of ${[...EFFORT_LEVELS].join(', ')}`;
  }
  return options;
}
export function extractQuestion(text) {
  if (typeof text !== 'string') return { clean: text, questions: null };
  const match = text.match(new RegExp(`\`\`\`${QUESTION_FENCE}\\s*\\n([\\s\\S]*?)\\n?\`\`\`\\s*$`));
  if (!match) return { clean: text, questions: null };
  let questions = null;
  try {
    const parsed = JSON.parse(match[1]);
    if (parsed && Array.isArray(parsed.questions) && parsed.questions.length) questions = parsed.questions;
  } catch { /* plain text, not a question */ }
  if (!questions) return { clean: text, questions: null };
  return { clean: text.slice(0, match.index).trimEnd(), questions };
}
export function normalizeQuestions(questions) {
  return (Array.isArray(questions) ? questions : []).map((q) => ({
    question: String(q?.question ?? ''),
    header: String(q?.header ?? ''),
    multi_select: Boolean(q?.multi_select ?? q?.multiSelect),
    options: (Array.isArray(q?.options) ? q.options : []).map((o) => ({
      label: String(o?.label ?? ''), description: String(o?.description ?? ''), preview: typeof o?.preview === 'string' ? o.preview : null,
    })),
  }));
}
export function toolResultText(text) {
  const str = typeof text === 'string' ? text : text == null ? '' : JSON.stringify(text);
  if (str.length <= MAX_TOOL_OUTPUT) return str;
  const suffix = `\n… [truncated ${str.length - MAX_TOOL_OUTPUT} characters]`;
  return str.slice(0, MAX_TOOL_OUTPUT - suffix.length) + suffix;
}
export function mcpResultText(result, error) {
  if (error?.message) return String(error.message);
  const content = result?.content;
  if (!Array.isArray(content)) return content == null ? '' : toolResultText(content);
  return toolResultText(content.map((p) => (typeof p?.text === 'string' ? p.text : JSON.stringify(p))).join('\n'));
}
export function formatAnswer(answers, response) {
  const lines = ['The user answered your question:'];
  for (const [question, answer] of Object.entries(answers ?? {})) lines.push(`- ${question}: ${Array.isArray(answer) ? answer.join(', ') : String(answer)}`);
  if (response) lines.push(`Free-text reply: ${response}`);
  return lines.join('\n');
}
// A tool-ish item → [name, input] for its tool_call; anything else maps elsewhere (text, question).
export function itemCall(item) {
  switch (item?.type) {
    case 'command_execution': return ['Bash', { command: item.command }];
    case 'file_change': return ['Edit', { changes: item.changes ?? [] }];
    case 'mcp_tool_call': return [`mcp__${item.server}__${item.tool}`, item.arguments && typeof item.arguments === 'object' ? item.arguments : {}];
    case 'web_search': return ['WebSearch', { query: item.query }];
    default: return null;
  }
}
// A tool-ish item → [output, isError] for its tool_result.
export function itemResult(item) {
  switch (item?.type) {
    case 'command_execution': return [item.aggregated_output ?? '', item.status === 'failed' || (item.exit_code ?? 0) !== 0];
    case 'file_change': return [(item.changes ?? []).map((c) => `${c.kind} ${c.path}`).join('\n') || `file changes ${item.status}`, item.status === 'failed'];
    case 'mcp_tool_call': return [mcpResultText(item.result, item.error), item.status === 'failed' || Boolean(item.error)];
    default: return [JSON.stringify({ query: item.query }), false]; // web_search carries no result payload
  }
}

// One Codex thread for the colony's lifetime. createCodex builds the client (injected for tests),
// commands are parsed stdin, emit writes one protocol event. False when preflight refused the run.
export async function runAgent({ createCodex, commands, emit, env = process.env, graceMs = 8000 }) {
  const pre = preflight(env);
  if (!pre.ok) {
    emit({ type: 'status', state: 'error', detail: pre.error });
    return false;
  }
  let status = null;
  const setStatus = (state, detail) => {
    if (state === status && detail === undefined) return;
    status = state;
    emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail });
  };
  let currentModel = codexModel(env.COLONIZER_MODEL); // '' is Codex's default
  let lastAnnounced = null;
  const pending = new Map();
  const totals = {}; // model -> cumulative tokens across turns
  let thread = null;
  let turnAbort = null;
  let turnPromise = null;
  let turnActive = false;
  let abortNext = false; // interrupt for a turn queued but not yet started (queueTurn defers by
  let turnsQueued = 0; // microtasks, so without this the abort lands on no controller)
  let closing = false;
  let messageCount = 0;
  let questionCount = 0;
  let firstTurn = true;
  const settleStatus = () => setStatus(pending.size > 0 ? 'waiting_for_answer' : turnActive ? 'working' : 'idle');
  const threadOptions = () => {
    const { warning, ...options } = buildThreadOptions(env, currentModel);
    if (warning) emit({ type: 'log', level: 'warn', message: warning });
    return options;
  };

  async function runTurn(codex, input) {
    const startedAt = Date.now();
    const ctrl = new AbortController();
    if (abortNext) {
      abortNext = false;
      ctrl.abort();
    }
    turnAbort = ctrl;
    turnActive = true;
    settleStatus();
    const openCalls = new Map();
    let lastMessage = { id: null, text: '', questions: null };
    let usage = null;
    let failure = null;
    const emitCall = (item) => {
      const [name, callInput] = itemCall(item);
      openCalls.set(`${item.id}`, true);
      emit({ type: 'tool_call', message_id: item.id, tool_call_id: `${item.id}`, name, input: callInput });
    };
    const emitResult = (item) => {
      const [output, isError] = itemResult(item);
      openCalls.delete(`${item.id}`);
      emit({ type: 'tool_result', tool_call_id: `${item.id}`, output: toolResultText(output), is_error: Boolean(isError) });
    };
    try {
      const { events } = await (thread ??= codex.startThread(threadOptions())).runStreamed(firstTurn ? `${SYSTEM_PROMPT_APPEND}\n\n${input}` : input, { signal: ctrl.signal });
      firstTurn = false;
      for await (const event of events) {
        const item = event?.item;
        if (event?.type === 'item.started') {
          if (itemCall(item)) emitCall(item);
        } else if (event?.type === 'item.completed' && item) {
          if (item.type === 'agent_message') {
            const found = extractQuestion(item.text);
            lastMessage = { id: item.id, text: found.clean, questions: found.questions };
            if (found.clean.trim()) emit({ type: 'assistant_text', message_id: item.id, block_index: 0, text: found.clean });
          } else if (item.type === 'reasoning') {
            if (item.text?.trim()) emit({ type: 'thinking', message_id: item.id, block_index: 0, text: item.text });
          } else if (item.type === 'todo_list') {
            const items = item.items ?? [];
            emit({ type: 'log', level: 'info', message: `todo: ${items.filter((t) => t.completed).length}/${items.length} steps done` });
          } else if (item.type === 'error') {
            emit({ type: 'log', level: 'warn', message: item.message });
          } else if (itemCall(item)) {
            if (!openCalls.has(`${item.id}`)) emitCall(item);
            emitResult(item);
          }
        } else if (event?.type === 'turn.completed') usage = event.usage ?? null;
        else if (event?.type === 'turn.failed') failure = event.error?.message ?? 'the turn failed';
        else if (event?.type === 'error') emit({ type: 'log', level: 'warn', message: event.message });
      }
    } catch (err) {
      failure ??= String(err?.message ?? err);
    } finally {
      turnAbort = null;
      turnActive = false;
    }
    for (const id of [...openCalls.keys()]) { // every tool_call gets its tool_result, even mid-item
      openCalls.delete(id);
      emit({ type: 'tool_result', tool_call_id: id, output: 'the turn ended before this call finished', is_error: true });
    }
    if (ctrl.signal.aborted) failure = 'interrupted'; // the signal kills the child, however the stream ends
    if (failure) {
      // The harness parks a colony whose result names quota exhaustion, so the message stays verbatim.
      emit({ type: 'log', level: failure === 'interrupted' ? 'info' : 'error', message: failure });
      emit({ type: 'turn_end', is_error: true, result: failure, cost_usd: null, duration_ms: Date.now() - startedAt });
    } else {
      if (usage) {
        const total = (totals[currentModel || 'codex'] ??= { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 });
        total.input_tokens += usage.input_tokens ?? 0;
        total.output_tokens += usage.output_tokens ?? 0;
        total.cache_read_tokens += usage.cached_input_tokens ?? 0;
        total.cache_write_tokens += usage.cache_write_input_tokens ?? 0;
      }
      if (lastMessage.questions) {
        const questionId = `q-${++questionCount}`;
        pending.set(questionId, true);
        emit({ type: 'question', question_id: questionId, message_id: lastMessage.id, questions: normalizeQuestions(lastMessage.questions) });
      }
      emit({
        type: 'turn_end', is_error: false, result: lastMessage.text || null, cost_usd: null, duration_ms: Date.now() - startedAt,
        ...(Object.keys(totals).length ? { model_usage: Object.fromEntries(Object.entries(totals).map(([name, tokens]) => [name, { ...tokens }])) } : {}),
      });
    }
    settleStatus();
  }

  // Telemetry stays off. `config` becomes --config overrides; `env` is never passed (it would replace process.env).
  const codex = createCodex({ config: { otel: { metrics_exporter: 'none' } } });
  setStatus('idle');
  if (currentModel) {
    emit({ type: 'model_changed', model: currentModel, previous: null });
    lastAnnounced = currentModel;
  }
  const queueTurn = (input) => { // chained, never awaited: the loop stays responsive to interrupt/shutdown
    turnsQueued += 1;
    turnPromise = (turnPromise ?? Promise.resolve()).catch(() => {}).then(() => {
      turnsQueued -= 1;
      if (!closing) return runTurn(codex, input);
      return null;
    });
  };

  commandLoop: for await (const command of commands) {
    switch (command?.type) {
      case 'user_message': {
        const text = typeof command.text === 'string' ? command.text : '';
        if (!text.trim()) {
          emit({ type: 'log', level: 'warn', message: 'ignored an empty user_message' });
          break;
        }
        emit({ type: 'user_message', id: typeof command.id === 'string' && command.id ? command.id : `u-${++messageCount}`, text });
        queueTurn(text);
        break;
      }
      case 'answer': {
        if (!pending.has(command.question_id)) {
          emit({ type: 'log', level: 'warn', message: `no open question with id ${command.question_id}` });
          break;
        }
        pending.delete(command.question_id);
        const answers = command.answers && typeof command.answers === 'object' && !Array.isArray(command.answers) ? command.answers : {};
        const response = typeof command.response === 'string' && command.response.trim() ? command.response : null;
        emit({ type: 'question_answered', question_id: command.question_id, answers, response });
        queueTurn(formatAnswer(answers, response));
        break;
      }
      case 'interrupt':
        if (turnAbort) turnAbort.abort();
        else if (turnsQueued > 0) abortNext = true; // nothing running or queued: nothing to stop
        break;
      case 'set_model': {
        const raw = typeof command.model === 'string' ? command.model.trim() : '';
        if (!raw) {
          emit({ type: 'log', level: 'warn', message: 'ignored a set_model without a model' });
          break;
        }
        const invalid = modelError(raw);
        if (invalid) {
          emit({ type: 'log', level: 'warn', message: `set_model ${raw} refused: ${invalid}` });
          break;
        }
        const previous = lastAnnounced;
        currentModel = codexModel(raw);
        if (thread?.id) {
          try {
            thread = codex.resumeThread(thread.id, threadOptions());
          } catch (err) {
            emit({ type: 'log', level: 'warn', message: `set_model ${raw} accepted but the thread would not resume: ${err?.message ?? err}` });
          }
        }
        emit({ type: 'model_changed', model: currentModel, previous });
        lastAnnounced = currentModel;
        break;
      }
      case 'shutdown':
        break commandLoop;
      default:
        break; // unknown commands are ignored (protocol forward compatibility)
    }
  }

  // Shutdown or stdin EOF. A resume boots a fresh microVM with a continue brief (the thread dies with
  // the VM, like Claude Code's SDK session), so no thread id is persisted.
  closing = true;
  turnAbort?.abort();
  if (turnPromise) await Promise.race([turnPromise.catch(() => {}), new Promise((resolve) => setTimeout(resolve, graceMs))]);
  pending.clear();
  setStatus('exited');
  return true;
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
  const { Codex } = await import('@openai/codex-sdk');
  // The exec child only reads CODEX_API_KEY, so an OPENAI_API_KEY-only colony still authenticates.
  const apiKey = process.env.CODEX_API_KEY?.trim() || process.env.OPENAI_API_KEY?.trim() || undefined;
  const ok = await runAgent({ createCodex: (options) => new Codex({ ...options, apiKey, codexPathOverride: process.env.COLONIZER_CODEX_BIN || undefined }), commands, emit });
  process.exit(ok ? 0 : 1);
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
