#!/usr/bin/env node
// Colonizer agent runner for the Pi coding agent (@earendil-works/pi-coding-agent): the runner
// contract of docs/protocol.md §2, with commands as JSON lines on stdin, events as JSON lines on
// stdout and diagnostics on stderr. Pi runs in RPC mode as a child process and reaches models only
// through the provider gateway, via a models.json this runner writes — a colony without a usable
// route never starts Pi at all.

import { spawn } from 'node:child_process';
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const MAX_TOOL_OUTPUT = 20_000;
// Pi's --thinking values; '' (no flag) is no thinking for gateway models.
export const EFFORT_LEVELS = new Set(['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']);

export const SYSTEM_PROMPT_APPEND = [
  'You are running inside a Colonizer colony: a disposable microVM whose work is published as a pull request. The user follows along in a web UI.',
  '- You are running unattended: nobody can answer a question mid-turn, so when a decision is yours to make, choose the reasonable option and say in your reply that you chose it.',
  '- Follow the brief you were given, including writing the pull request description to /harness/out/pr.md.',
  '- You have only your built-in tools (read, bash, edit, write) — no subagents and no Colonizer tools. Where the brief names a tool you do not have, such as filing a finding, do the work yourself (run the command, note it in your reply) instead of trying to call it.',
].join('\n');

const AUTH_MODES = new Set(['x-api-key', 'bearer', 'none']);
const HEADER_NAME = /^[A-Za-z0-9-]+$/;
const positiveInt = (value) => (Number.isInteger(value) && value > 0 ? value : null);

/**
 * COLONIZER_MODEL_ROUTES → validated routes, with warnings instead of throwing. Same route JSON as
 * the claude-code module's router.mjs, validated again here because a module is an independent
 * directory.
 */
export function parseRoutes(raw) {
  let data;
  const bad = (why) => ({ routes: [], warnings: [`ignoring COLONIZER_MODEL_ROUTES: ${why}; no provider is reachable`] });
  try {
    data = JSON.parse(raw);
  } catch {
    return bad('not valid JSON');
  }
  if (!Array.isArray(data)) return bad('expected a JSON array');
  const routes = [];
  const warnings = [];
  data.forEach((entry, i) => {
    const prefix = typeof entry?.prefix === 'string' ? entry.prefix : '';
    const baseUrl = typeof entry?.base_url === 'string' ? entry.base_url : '';
    const auth = entry?.auth ?? 'x-api-key';
    if (!prefix.endsWith('/') || prefix.length < 2 || !/^https?:\/\//.test(baseUrl) || !AUTH_MODES.has(auth)) {
      warnings.push(`ignoring model route ${i}: needs a "<provider>/" prefix, an http(s) base_url and auth x-api-key|bearer|none`);
      return;
    }
    const headers = {};
    if (entry.headers && typeof entry.headers === 'object' && !Array.isArray(entry.headers)) {
      for (const [name, header] of Object.entries(entry.headers)) {
        if (HEADER_NAME.test(name) && typeof header === 'string') headers[name.toLowerCase()] = header;
      }
    }
    routes.push({
      provider: typeof entry.provider === 'string' && entry.provider ? entry.provider : prefix.slice(0, -1),
      prefix,
      base_url: baseUrl,
      headers,
      context_tokens: positiveInt(entry.context_tokens),
    });
  });
  return { routes, warnings };
}

/** The route a `<provider>/<model>` setting belongs to, split into parts, or null when it matches none. */
function resolveModel(routes, model) {
  const name = typeof model === 'string' ? model.trim() : '';
  const slash = name.indexOf('/');
  if (slash < 1 || slash === name.length - 1) return null;
  const route = routes.find((entry) => entry.prefix === `${entry.provider}/` && name.startsWith(entry.prefix));
  return route ? { route, provider: name.slice(0, slash), modelId: name.slice(slash + 1) } : null;
}

/**
 * Why the colony cannot run, or null when it can. Pi has no model of its own and no Anthropic
 * credential inside the VM, so an unusable model setting is a stop, not a fallback.
 */
export function selectionProblem(routes, model) {
  const action = 'Pi reaches models only through the provider gateway: set the Pi agent\'s model to <provider>/<model> for a provider configured in Settings → Providers.';
  if (!routes.length) return `no provider is configured, so Pi has no model to run on. ${action}`;
  const name = typeof model === 'string' ? model.trim() : '';
  if (!name) return `the model setting is empty, so Pi has no model to run on. ${action}`;
  if (!resolveModel(routes, name)) {
    return `${name} matches none of the configured providers (${routes.map((route) => route.prefix).join(', ')}). ${action}`;
  }
  return null;
}

/**
 * The models.json Pi loads from PI_CODING_AGENT_DIR: the selected model on its gateway provider,
 * speaking the anthropic-messages wire the gateway always presents to guests. apiKey is a
 * placeholder Pi needs to consider the provider authenticated — the gateway drops an incoming
 * x-api-key and authenticates the colony by the x-colonizer-colony header instead. `reasoning` is
 * set only when thinking is requested: a reasoning model without --thinking defaults to a thinking
 * level, a non-reasoning one clamps it.
 */
export function buildModelsConfig(routes, model, effort = '') {
  const { route, modelId } = resolveModel(routes, model);
  return {
    providers: {
      [route.provider]: {
        baseUrl: route.base_url,
        api: 'anthropic-messages',
        apiKey: 'colonizer-gateway',
        ...(Object.keys(route.headers).length ? { headers: route.headers } : {}),
        models: [{ id: modelId, name: modelId, api: 'anthropic-messages', ...(EFFORT_LEVELS.has(effort) && effort !== 'off' ? { reasoning: true } : {}), ...(route.context_tokens ? { contextWindow: route.context_tokens } : {}) }],
      },
    },
  };
}

/** The Pi command line: RPC mode, no session or loadable extras, the gateway model, the colony note. */
export function piArgs({ provider, modelId, effort = '' }) {
  return ['--no-session', '--no-extensions', '--no-skills', '--no-prompt-templates', '--provider', provider, '--model', modelId, ...(EFFORT_LEVELS.has(effort) ? ['--thinking', effort] : []), '--append-system-prompt', SYSTEM_PROMPT_APPEND];
}

/**
 * Pi's environment: the PI_* flags stop the startup network (model-catalog refresh, version check,
 * telemetry), and COLONIZER_MODEL_ROUTES — the colony's gateway token, which Pi and the shell
 * commands it runs must never see — is stripped.
 */
export function piEnv(env, agentDir) {
  const out = { ...env };
  delete out.COLONIZER_MODEL_ROUTES;
  out.PI_CODING_AGENT_DIR = agentDir;
  out.PI_OFFLINE = '1';
  out.PI_SKIP_VERSION_CHECK = '1';
  out.PI_TELEMETRY = '0';
  return out;
}

// The package's exports map blocks `pkg/package.json`, so the bin path cannot be resolved the way
// the claude-code module resolves its SDK; the map exposes this entry script instead: the same CLI
// with --mode rpc prepended.
const PI_RPC_ENTRY = fileURLToPath(new URL(import.meta.resolve('@earendil-works/pi-coding-agent/rpc-entry')));
const spawnPiDefault = ({ args, env, cwd }) => spawn(process.execPath, [PI_RPC_ENTRY, ...args], { cwd, env, stdio: ['pipe', 'pipe', 'pipe'] });

/** tool_execution_end result → display text, capped at MAX_TOOL_OUTPUT characters. */
export function toolResultText(result) {
  let text;
  const content = result?.content;
  if (typeof result === 'string') text = result;
  else if (Array.isArray(content)) {
    text = content.map((part) => (part?.type === 'text' ? part.text : part?.type === 'image' ? '[image]' : JSON.stringify(part))).join('\n');
  } else text = result == null ? '' : JSON.stringify(result);
  if (text.length <= MAX_TOOL_OUTPUT) return text;
  const suffix = `\n… [truncated ${text.length - MAX_TOOL_OUTPUT} characters]`;
  return text.slice(0, MAX_TOOL_OUTPUT - suffix.length) + suffix;
}

/** Splits a byte stream into lines on LF only; a trailing partial line waits for more bytes. */
export function lfSplitter(onLine) {
  let buffer = Buffer.alloc(0);
  return (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    let index;
    while ((index = buffer.indexOf(0x0a)) !== -1) {
      let line = buffer.subarray(0, index);
      buffer = buffer.subarray(index + 1);
      if (line.length && line[line.length - 1] === 0x0d) line = line.subarray(0, line.length - 1);
      onLine(line.toString('utf8'));
    }
  };
}

/** Minimal async queue usable as an AsyncIterable (stdin commands). */
export function commandQueue() {
  const items = [];
  const waiters = [];
  let closed = false;
  return {
    push(value) {
      const waiter = waiters.shift();
      if (waiter) waiter({ value, done: false });
      else items.push(value);
    },
    close() {
      closed = true;
      for (const waiter of waiters.splice(0)) waiter({ value: undefined, done: true });
    },
    [Symbol.asyncIterator]() {
      return {
        next: () => (items.length ? Promise.resolve({ value: items.shift(), done: false }) : closed ? Promise.resolve({ value: undefined, done: true }) : new Promise((resolve) => waiters.push(resolve))),
      };
    },
  };
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Drives one Pi RPC process through the runner contract. `spawnPi` (the real spawn, injected for
 * tests) gets Pi's args and env; `selection` is what the model setting resolved to.
 */
export async function runAgent({ spawnPi = spawnPiDefault, commands, emit, selection, effort = '', env = process.env, cwd = process.cwd(), graceMs = 5000 }) {
  let status = null;
  const setStatus = (state, detail) => {
    if (state === status && detail === undefined) return;
    status = state;
    emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail });
  };

  let turnActive = false, closing = false, dead = false, turnStartedAt = null;
  let messageCount = 0, requestSeq = 0; // pi-N assistant messages, u-N fallback user ids, req-N requests
  let promptsOutstanding = 0, promptInFlight = false, currentMessageId = null, costUsd = 0;
  const pendingPrompts = []; // texts queued behind the prompt whose acceptance response has not arrived
  let lastAssistant = { stopReason: null, errorMessage: null, text: null };
  const startupModel = `${selection.route.provider}/${selection.modelId}`;
  const streams = new Map(); // message_id -> Map<contentIndex, {type, id, final}>
  const fallbackIndex = new Map(); // message_id -> next index when nothing was streamed
  const modelUsage = new Map(); // "provider/model" -> cumulative tokens
  const pendingResponses = new Map(); // request id → response handler

  const child = spawnPi({ args: piArgs({ ...selection, effort }), env, cwd });
  child.stdin.on('error', () => {}); // Pi gone: the exit path reports it, not a broken pipe

  /**
   * Sends a command and hands its response record to `onResponse` — synchronously as the record is
   * read, never in a later microtask. Records often coalesce into one chunk: a prompt's response
   * and the same run's agent_settled arrive together, and the acceptance bookkeeping must happen
   * before the settled side of the chunk is processed.
   */
  const request = (payload, onResponse = () => {}) => {
    const id = `req-${++requestSeq}`;
    pendingResponses.set(id, onResponse);
    child.stdin.write(`${JSON.stringify({ ...payload, id })}\n`);
  };

  const accumulateUsage = (message) => {
    const usage = message?.usage;
    if (!usage || typeof usage !== 'object' || typeof message.provider !== 'string' || typeof message.model !== 'string') return;
    const n = (value) => (Number.isFinite(value) ? value : 0);
    const key = `${message.provider}/${message.model}`;
    const entry = modelUsage.get(key) ?? { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 };
    entry.input_tokens += n(usage.input);
    entry.output_tokens += n(usage.output);
    entry.cache_read_tokens += n(usage.cacheRead);
    entry.cache_write_tokens += n(usage.cacheWrite);
    modelUsage.set(key, entry);
    costUsd += n(usage.cost?.total);
  };

  // The turn_end body for the state Pi leaves a settled turn in; an aborted turn is an error too:
  // the only aborts are our own interrupt and the shutdown path, and a colony whose turn was cut
  // short should read as interrupted, not as finished work.
  const finishTurn = (final = lastAssistant, { idle = true } = {}) => {
    promptsOutstanding = 0;
    if (!turnActive) return;
    turnActive = false;
    const isError = final.stopReason === 'error' || final.stopReason === 'aborted';
    emit({
      type: 'turn_end',
      is_error: isError,
      result: isError ? final.errorMessage ?? final.text ?? null : final.text ?? final.errorMessage ?? null,
      cost_usd: Math.round(costUsd * 1e6) / 1e6,
      duration_ms: turnStartedAt == null ? null : Date.now() - turnStartedAt,
      ...(modelUsage.size ? { model_usage: Object.fromEntries(modelUsage) } : {}),
    });
    turnStartedAt = null;
    lastAssistant = { stopReason: null, errorMessage: null, text: null };
    streams.clear();
    fallbackIndex.clear();
    currentMessageId = null;
    if (idle) setStatus('idle');
  };

  // Complete assistant messages may drop streamed blocks (a provider that does not replay thinking),
  // so the streamed contentIndex is recovered by matching, the way the claude-code runner does.
  const blockIndex = (messageId, block) => {
    const blocks = streams.get(messageId);
    if (blocks?.size) {
      for (const [index, slot] of blocks) {
        if (!slot.final && slot.type === block.type && (block.type !== 'toolCall' || slot.id === block.id)) return ((slot.final = true), index);
      }
      for (const [index, slot] of blocks) {
        if (!slot.final && slot.type === block.type) return ((slot.final = true), index);
      }
    }
    const next = fallbackIndex.get(messageId) ?? (blocks?.size ? Math.max(...blocks.keys()) + 1 : 0);
    fallbackIndex.set(messageId, next + 1);
    return next;
  };

  const blockSlot = (messageId, index) => {
    const blocks = streams.get(messageId) ?? new Map();
    streams.set(messageId, blocks);
    const slot = blocks.get(index) ?? { type: null, id: null, final: false };
    blocks.set(index, slot);
    return slot;
  };

  const onAssistantEnd = (message) => {
    const messageId = currentMessageId;
    if (!messageId) return;
    accumulateUsage(message);
    const textParts = [];
    for (const block of Array.isArray(message.content) ? message.content : []) {
      const index = blockIndex(messageId, block);
      if (block.type === 'text' && block.text) {
        textParts.push(block.text);
        emit({ type: 'assistant_text', message_id: messageId, block_index: index, text: block.text });
      } else if (block.type === 'thinking' && String(block.thinking ?? '').trim()) {
        emit({ type: 'thinking', message_id: messageId, block_index: index, text: block.thinking });
      } else if (block.type === 'toolCall' && typeof block.id === 'string') {
        emit({ type: 'tool_call', message_id: messageId, tool_call_id: block.id, name: String(block.name ?? 'unknown'), input: block.arguments && typeof block.arguments === 'object' && !Array.isArray(block.arguments) ? block.arguments : {} });
      }
    }
    lastAssistant = {
      stopReason: typeof message.stopReason === 'string' ? message.stopReason : null,
      errorMessage: typeof message.errorMessage === 'string' && message.errorMessage ? message.errorMessage : null,
      text: textParts.join('\n') || null,
    };
  };

  const onRecord = (record) => {
    if (record?.type === 'response') {
      const handler = pendingResponses.get(record.id);
      pendingResponses.delete(record.id);
      if (handler) handler(record);
    } else if (record?.type === 'agent_start') {
      // A queued prompt's run can start after the previous one settled, so an agent_start with work
      // outstanding reopens the turn (finishTurn may have closed it between the two runs).
      if (promptsOutstanding > 0 && !turnActive) {
        turnActive = true;
        setStatus('working');
      }
    } else if (record?.type === 'message_start') {
      if (record.message?.role === 'assistant') currentMessageId = `pi-${++messageCount}`;
    } else if (record?.type === 'message_update') {
      const event = record.assistantMessageEvent;
      if (!event || !currentMessageId) return;
      if (event.type === 'text_delta' && event.delta) {
        blockSlot(currentMessageId, event.contentIndex).type ??= 'text';
        emit({ type: 'assistant_text_delta', message_id: currentMessageId, block_index: event.contentIndex, delta: event.delta });
      } else if (event.type === 'thinking_delta') {
        // No thinking_delta event exists in the runner contract; the final thinking block carries it.
        blockSlot(currentMessageId, event.contentIndex).type ??= 'thinking';
      } else if (event.type === 'toolcall_start') {
        Object.assign(blockSlot(currentMessageId, event.contentIndex), { type: 'toolCall', id: event.id ?? null });
      }
    } else if (record?.type === 'message_end') {
      if (record.message?.role === 'assistant') onAssistantEnd(record.message);
    } else if (record?.type === 'tool_execution_end' && typeof record.toolCallId === 'string') {
      emit({ type: 'tool_result', tool_call_id: record.toolCallId, output: toolResultText(record.result), is_error: Boolean(record.isError) });
    } else if (record?.type === 'agent_settled') {
      finishTurn();
    }
    // turn_start/turn_end/agent_end and unknown records carry nothing the contract needs
  };

  child.stdout.on('data', lfSplitter((line) => {
    if (!line.trim()) return;
    try {
      onRecord(JSON.parse(line));
    } catch {
      emit({ type: 'log', level: 'warn', message: 'ignored a pi output line that is not valid JSON' });
    }
  }));
  // Pi's diagnostics: forwarded as log events, never raw to stdout (that stream is protocol-only).
  child.stderr.on('data', lfSplitter((line) => {
    if (line.trim()) emit({ type: 'log', level: 'warn', message: `pi: ${line.trim()}` });
  }));

  // An unexpected exit ends the session — and so does a spawn that failed, whose 'error' event may
  // be all that ever fires: without it a failed spawn hangs on commands that cannot be answered.
  const crashed = new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve(`pi exited unexpectedly (${signal ? `signal ${signal}` : `code ${code}`})`));
    child.once('error', (error) => resolve(`pi failed to start (${error?.message ?? String(error)})`));
  });
  crashed.then((detail) => {
    if (closing || dead) return;
    dead = true;
    emit({ type: 'log', level: 'error', message: detail });
    finishTurn({ stopReason: 'error', errorMessage: lastAssistant.errorMessage ?? detail, text: lastAssistant.text }, { idle: false });
    setStatus('error', detail);
    setStatus('exited');
  });

  setStatus('idle');
  emit({ type: 'model_changed', model: startupModel, previous: null });
  emit({ type: 'log', level: 'info', message: `pi started on ${startupModel} in ${cwd}` });

  // One prompt on pi's stdin at a time: a second prompt written before the first one's acceptance
  // response arrives is dropped by pi (with a spurious agent_settled), so queued messages wait for
  // it. A prompt joins the running turn as a followUp — a bare prompt is refused while pi streams —
  // and is bare once nothing accepted is still unsettled.
  const sendNextPrompt = () => {
    if (promptInFlight || !pendingPrompts.length) return;
    promptInFlight = true;
    const message = pendingPrompts.shift();
    request({ type: 'prompt', message, ...(promptsOutstanding > 0 ? { streamingBehavior: 'followUp' } : {}) }, (response) => {
      promptInFlight = false;
      if (response.success) {
        promptsOutstanding += 1;
      } else {
        const reason = `pi refused the message: ${response.error ?? 'unknown error'}`;
        emit({ type: 'log', level: 'error', message: reason });
        // A refusal with nothing accepted leaves no run that could ever settle the turn.
        if (promptsOutstanding === 0 && turnActive) finishTurn({ stopReason: 'error', errorMessage: reason, text: null });
      }
      sendNextPrompt();
    });
  };

  const loop = (async () => {
    for await (const command of commands) {
      if (dead || closing) break;
      if (command?.type === 'user_message') {
        const text = typeof command.text === 'string' ? command.text : '';
        if (!text.trim()) {
          emit({ type: 'log', level: 'warn', message: 'ignored an empty user_message' });
          continue;
        }
        emit({ type: 'user_message', id: typeof command.id === 'string' && command.id ? command.id : `u-${++messageCount}`, text });
        turnActive = true;
        turnStartedAt ??= Date.now();
        setStatus('working');
        pendingPrompts.push(text);
        sendNextPrompt();
      } else if (command?.type === 'answer') {
        emit({ type: 'log', level: 'warn', message: 'ignored an answer: Pi has no way to ask questions' });
      } else if (command?.type === 'interrupt') {
        request({ type: 'abort' }, (response) => {
          if (!response.success) emit({ type: 'log', level: 'warn', message: `interrupt failed: ${response.error ?? 'unknown error'}` });
        });
      } else if (command?.type === 'set_model') {
        const model = typeof command.model === 'string' ? command.model.trim() : '';
        // set_model resolves against the models Pi loaded at startup: models.json is not re-read,
        // and it lists only the selected model, so nothing else can be switched to in this session.
        if (model !== startupModel) {
          emit({ type: 'log', level: 'warn', message: `set_model ${model || '(empty)'} failed: pi read models.json only at startup and knows just ${startupModel}; leaving the model as it is` });
        } else {
          request({ type: 'set_model', provider: selection.route.provider, modelId: selection.modelId }, (response) => {
            if (response.success) emit({ type: 'model_changed', model, previous: startupModel });
            else emit({ type: 'log', level: 'warn', message: `set_model ${model} failed: ${response.error ?? 'unknown error'}` });
          });
        }
      } else if (command?.type === 'shutdown') {
        return 'shutdown';
      } // unknown commands are ignored (protocol forward compatibility)
    }
    return 'eof';
  })();

  const outcome = await Promise.race([loop, crashed.then(() => 'crash')]);
  closing = true;
  if (outcome !== 'crash' && !dead) {
    child.stdin.end(); // an orderly shutdown: closing stdin asks Pi to dispose and exit on its own
    if (!(await Promise.race([crashed, sleep(graceMs).then(() => null)]))) {
      child.kill('SIGTERM');
      await Promise.race([crashed, sleep(2000)]);
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    }
    setStatus('exited');
  }
  await Promise.race([crashed, sleep(100)]); // let the exit bookkeeping settle before returning
}

/**
 * The no-route colony: Pi is never started. Each user_message is answered with a failed turn so
 * the colony stops cleanly instead of hanging on a turn that can never run.
 */
export async function runUnconfigured({ commands, emit, detail }) {
  emit({ type: 'log', level: 'error', message: detail });
  emit({ type: 'status', state: 'error', detail });
  let count = 0;
  for await (const command of commands) {
    if (command?.type === 'user_message') {
      const text = typeof command.text === 'string' ? command.text : '';
      if (!text.trim()) {
        emit({ type: 'log', level: 'warn', message: 'ignored an empty user_message' });
      } else {
        emit({ type: 'user_message', id: typeof command.id === 'string' && command.id ? command.id : `u-${++count}`, text });
        emit({ type: 'turn_end', is_error: true, result: detail, cost_usd: 0, duration_ms: 0 });
      }
    } else if (command?.type === 'set_model') {
      emit({ type: 'log', level: 'warn', message: 'ignored a set_model: no provider is configured' });
    } else if (command?.type === 'answer') {
      emit({ type: 'log', level: 'warn', message: 'ignored an answer: Pi has no way to ask questions' });
    } else if (command?.type === 'shutdown') {
      break;
    }
  }
  emit({ type: 'status', state: 'exited' });
}

async function main() {
  const emit = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
  const commands = commandQueue();

  // The command stream gets the same LF-only framing as Pi's streams.
  process.stdin.on('data', lfSplitter((line) => {
    if (!line.trim()) return;
    try {
      commands.push(JSON.parse(line));
    } catch {
      emit({ type: 'log', level: 'warn', message: 'ignored a command line that is not valid JSON' });
    }
  }));
  process.stdin.on('end', () => commands.close());
  for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => commands.push({ type: 'shutdown' }));

  const { routes, warnings } = parseRoutes(process.env.COLONIZER_MODEL_ROUTES ?? '');
  for (const message of warnings) emit({ type: 'log', level: 'warn', message });

  const model = (process.env.COLONIZER_MODEL ?? '').trim();
  const problem = selectionProblem(routes, model);
  if (problem) {
    await runUnconfigured({ commands, emit, detail: problem });
    process.exit(0);
  }

  const effort = (process.env.COLONIZER_EFFORT ?? '').trim();
  if (effort && !EFFORT_LEVELS.has(effort)) emit({ type: 'log', level: 'warn', message: `ignoring COLONIZER_EFFORT=${effort}; expected one of ${['', ...EFFORT_LEVELS].join(', ')}` });

  // The module directory is mounted read-only, so Pi's configuration lives in a fresh private
  // directory: models.json is written 0600 and holds the colony's gateway token in its headers.
  const agentDir = mkdtempSync(join(tmpdir(), 'colonizer-pi-'));
  writeFileSync(join(agentDir, 'models.json'), `${JSON.stringify(buildModelsConfig(routes, model, effort))}\n`, { mode: 0o600 });
  try {
    await runAgent({ commands, emit, selection: resolveModel(routes, model), effort: EFFORT_LEVELS.has(effort) ? effort : '', env: piEnv(process.env, agentDir), cwd: process.cwd() });
  } finally {
    rmSync(agentDir, { recursive: true, force: true });
  }
  process.exit(0);
}

// Run only as a script (node runner.mjs), not under the tests' import.
try {
  if (realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
    main().catch((err) => {
      process.stderr.write(`${err?.stack ?? err}\n`);
      process.stdout.write(`${JSON.stringify({ type: 'status', state: 'exited', detail: String(err?.message ?? err) })}\n`);
      process.exit(1);
    });
  }
} catch {}
