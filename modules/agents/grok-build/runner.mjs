#!/usr/bin/env node
// Colonizer agent runner for xAI's Grok Build CLI (`grok`), headless: the runner contract of
// docs/protocol.md §2 (JSON-line commands on stdin, JSON-line protocol events on stdout). One
// `grok` child per turn; the first turn's `end` event carries the grok sessionId and every later
// turn resumes it with `-r`, so a colony is one continuous grok session. Questions go through our
// own MCP tool (mcp.mjs, registered in the fresh GROK_HOME's config.toml) and a loopback bridge
// below. The pin lives in module.json; every flag and event field is cited from the upstream user
// guide in the README.

import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

// The named preflight problems (README, "Preflight"): each is the detail on the `status error`
// event and the prefix of the log that says how to fix it.
export const MISSING_CREDENTIAL = 'GROK_CREDENTIAL_MISSING';
export const MISSING_BINARY = 'GROK_BINARY_MISSING';
export const VERSION_DRIFT = 'GROK_VERSION_DRIFT';
export const MODEL_PROVIDER = 'GROK_MODEL_PROVIDER';

/** The pin, read from module.json so the manifest and this preflight cannot drift apart. */
export function readPin() {
  const pin = JSON.parse(readFileSync(join(here, 'module.json'), 'utf8'))?.requires?.pins?.grok;
  if (!pin?.version) throw new Error('module.json carries no grok pin');
  return pin;
}

/** Where the grok binary is: COLONIZER_GROK_BIN wins, else `grok` on the PATH. */
export function grokBin(env) {
  return String(env.COLONIZER_GROK_BIN ?? '').trim() || 'grok';
}

/** The first X.Y.Z in `grok --version`'s output, whatever else surrounds it. */
export function parseVersion(text) {
  const m = /\b(\d+)\.(\d+)\.(\d+)\b/.exec(String(text ?? ''));
  return m ? `${m[1]}.${m[2]}.${m[3]}` : null;
}

/** `grok --version`'s output: undefined when the binary could not run, null when it ran and failed. */
function versionOf(bin, spawnFn) {
  return new Promise((resolve) => {
    let out = '';
    const child = spawnFn(bin, ['--version'], { stdio: ['ignore', 'pipe', 'pipe'] });
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => (out += chunk));
    child.on('error', () => resolve(undefined));
    child.on('close', (code) => resolve(code === 0 ? out : null));
  });
}

/** The checks that must pass before any grok process is spawned: fail loudly, not on a prompt. */
export async function preflight({ env, spawnFn = spawn, pin = readPin() }) {
  if (!String(env.XAI_API_KEY ?? '').trim()) {
    return {
      code: MISSING_CREDENTIAL,
      message:
        'XAI_API_KEY is unset or empty, and the colony never runs browser OAuth (grok login). ' +
        'Add an xAI API key from console.x.ai as a colony secret named XAI_API_KEY for host api.x.ai, ' +
        'so the mothership injects it into this colony (README, "Credential story").',
    };
  }
  const bin = grokBin(env);
  const text = await versionOf(bin, spawnFn);
  if (text === undefined) {
    return { code: MISSING_BINARY, message: `the grok CLI was not found at "${bin}". Install the pinned version: curl -fsSL ${pin.install} | bash -s ${pin.version}` };
  }
  const found = parseVersion(text);
  if (found !== pin.version) {
    return {
      code: VERSION_DRIFT,
      message:
        `grok --version printed "${String(text).trim()}" (parsed ${found ?? 'nothing'}) instead of the pinned ` +
        `${pin.version} (SOURCE_REV ${pin.source_rev}). Install the pinned version: curl -fsSL ${pin.install} | bash -s ${pin.version}`,
    };
  }
  return null;
}

/** The `-m` value for a model setting: `xai-grok/<model>` or a bare xAI model id; any other
 * provider prefix is refused by name (gateway routing is a follow-up, README "Credential story"). */
export function resolveModel(spec) {
  const value = String(spec ?? '').trim();
  if (!value) return {};
  const slash = value.indexOf('/');
  if (slash > 0 && value.slice(0, slash) !== 'xai-grok') {
    return { error: `${MODEL_PROVIDER}: "${value}" names another provider; this module runs xai-grok/<model> only` };
  }
  return { model: slash > 0 ? value.slice(slash + 1) : value };
}

/** One headless grok turn (14-headless-mode.md), with the nesting decisions from the README applied. */
export function turnArgs({ promptFile, model, sessionId }) {
  const args = [
    '--prompt-file', promptFile,
    '--output-format', 'streaming-json',
    '--always-approve', // headless cannot answer a permission prompt; the microVM is the boundary
    '--sandbox', 'off', // grok's own OS sandbox stays off; the microVM is the colony's boundary
    '--disable-web-search', // the web-search backend's host is unverified, and egress denies it anyway
    '--no-auto-update',
  ];
  if (model) args.push('-m', model);
  if (sessionId) args.push('-r', sessionId); // resume: a colony is one continuous grok session
  return args;
}

/** The child's environment. A fresh GROK_HOME is the one nesting lever the docs verify: grok's
 * config, cached OAuth token, skills and MCP definitions all live under it (README "Nesting"). */
export function childEnv(env, home) {
  return {
    ...env,
    BROWSER: '/bin/false', // belt-and-braces: nothing may open a browser, and this runner never runs grok login
    GROK_HOME: home,
    GROK_MEMORY: '0', // no cross-session memory (05-configuration.md)
    GROK_TELEMETRY_ENABLED: '0', // product analytics off (05-configuration.md)
    GROK_DISABLE_AUTOUPDATER: '1', // no update checks inside a colony (14-headless-mode.md)
  };
}

/** The fresh GROK_HOME's config.toml: it registers the vendored colonizer MCP server so the model
 * can call colonizer__ask_user, with the loopback bridge's URL and token in the server's env
 * (README "Questions"). JSON.stringify of a string is a valid TOML basic string for ordinary paths
 * and URLs. tool_timeout_sec is set to an hour explicitly: an ask waits on a human, and grok's own
 * default is 6000 s per tool call. */
export function homeConfig({ bridge, node = process.execPath, mcpModule = join(here, 'mcp.mjs') }) {
  return [
    '[mcp_servers.colonizer]',
    `command = ${JSON.stringify(node)}`,
    `args = [${JSON.stringify(mcpModule)}]`,
    `env = { COLONIZER_BRIDGE_URL = ${JSON.stringify(bridge.url)}, COLONIZER_BRIDGE_TOKEN = ${JSON.stringify(bridge.token)} }`,
    'tool_timeout_sec = 3600',
  ].join('\n');
}

/** Loopback HTTP bridge to mcp.mjs: an ask_user call parks until the matching `answer` command.
 * Interrupt, turn end and shutdown cancel the parked calls; `answer` on an unknown id is a warn. */
export async function createBridge({ emit, setStatus, isWorking, token = randomBytes(16).toString('hex') }) {
  let count = 0;
  const pending = new Map();
  const server = createServer((req, res) => {
    const reply = (status, payload) => { if (res.destroyed) return; res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(payload)); };
    if (req.method !== 'POST' || req.headers.authorization !== `Bearer ${token}`) { reply(req.method !== 'POST' ? 404 : 401, {}); return; }
    let body = '';
    req.on('data', (c) => { body += c; if (body.length > 1 << 20) req.destroy(); });
    req.on('end', () => {
      let msg = null;
      try { msg = JSON.parse(body || '{}'); } catch { reply(400, {}); return; }
      if (req.url !== '/ask') { reply(404, {}); return; }
      const questionId = `q-${++count}`;
      pending.set(questionId, (answers) => reply(200, answers ?? { cancelled: true }));
      const qs = Array.isArray(msg.questions) ? msg.questions : [];
      emit({ type: 'question', question_id: questionId, message_id: typeof msg.message_id === 'string' ? msg.message_id : null, questions: qs.map((q) => ({ question: String(q?.question ?? ''), header: String(q?.header ?? q?.question ?? ''), multi_select: Boolean(q?.multiSelect), options: (Array.isArray(q?.options) ? q.options : []).map((o) => ({ label: String(o?.label ?? ''), description: String(o?.description ?? ''), preview: null })) })) });
      setStatus('waiting_for_answer');
    });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  return {
    url: `http://127.0.0.1:${server.address().port}`, token, pending: () => pending.size,
    answer(questionId, answers, response) {
      const resolve = pending.get(questionId);
      if (!resolve) return false;
      pending.delete(questionId);
      emit({ type: 'question_answered', question_id: questionId, answers, response });
      if (pending.size) setStatus('waiting_for_answer');
      else setStatus(isWorking() ? 'working' : 'idle');
      resolve({ answers, response });
      return true;
    },
    cancelAll() { for (const resolve of pending.values()) resolve(null); pending.clear(); },
    close: () => new Promise((resolve) => { server.close(resolve); server.closeAllConnections?.(); }),
  };
}

// tool_call_update statuses that are progress, not a result; anything else is the tool's outcome.
const PROGRESS = new Set(['in_progress', 'pending', 'running']);
// docs/protocol.md §2 caps tool_result output at 20 000 characters.
const TOOL_RESULT_LIMIT = 20000;

const clip = (text, limit = TOOL_RESULT_LIMIT) => (text.length > limit ? `${text.slice(0, limit - 1)}…` : text);
const plainObject = (value) => (value && typeof value === 'object' && !Array.isArray(value) ? value : {});
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** A tool_call_update's payload as displayable text: the raw output, plus any content blocks. */
function toolOutput(update) {
  const parts = [];
  if (update.rawOutput !== undefined && update.rawOutput !== null) {
    parts.push(typeof update.rawOutput === 'string' ? update.rawOutput : JSON.stringify(update.rawOutput));
  }
  for (const block of Array.isArray(update.content) ? update.content : []) {
    parts.push(typeof block?.text === 'string' ? block.text : JSON.stringify(block));
  }
  return parts.join('\n') || '{}';
}

/** grok's `end`/`error` spend fields, accumulated across turns into the colony-cumulative totals
 * turn_end wants (§2): one grok process per turn means each event carries only that turn's spend. */
export function mergeUsage(totals, event) {
  if (Number.isFinite(event?.total_cost_usd)) totals.cost = (totals.cost ?? 0) + event.total_cost_usd;
  for (const [model, usage] of Object.entries(event?.modelUsage ?? {})) {
    if (!usage || typeof usage !== 'object') continue;
    const soFar = (totals.models[model] ??= { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 });
    soFar.input_tokens += usage.inputTokens ?? 0;
    soFar.output_tokens += usage.outputTokens ?? 0;
    soFar.cache_read_tokens += usage.cacheReadInputTokens ?? 0;
    soFar.cache_write_tokens += usage.cacheCreationInputTokens ?? 0;
    // §2: with model_usage present, cost_usd sums only the models the provider itself prices
    // (keys without a '/'), like the claude-code runner's Claude-only cost.
    if (!model.includes('/') && Number.isFinite(usage.costUSD)) totals.modelCost = (totals.modelCost ?? 0) + usage.costUSD;
  }
}

/** Runs one turn as a grok child, emitting the mapped protocol events as they arrive; resolves with
 * the turn's grok sessionId once the child exits. `interrupt()` SIGINTs the child (grok saves
 * session state and exits 130) and escalates to SIGKILL after a grace period; an interrupt that
 * lands before the spawn (while the prompt file is being written) skips the spawn and ends the
 * turn as interrupted at once. */
export function startTurn({ prompt, model, sessionId, messageId, env, home, emit, spawnFn = spawn, totals }) {
  let child = null;
  let interrupted = false;
  const done = (async () => {
    const startedAt = Date.now();
    // --prompt-file over -p: an issue brief can be far larger than an argv slot.
    const promptFile = join(home, `${messageId}.prompt.txt`);
    await writeFile(promptFile, prompt, 'utf8');
    // An interrupt that landed while the prompt file was being written had no child to signal,
    // so it is only recorded; honor it here, at the spawn point: grok never starts, and the turn
    // ends as interrupted exactly like a child that caught the SIGINT mid-run.
    if (interrupted) {
      rmSync(promptFile, { force: true });
      const hasModels = Object.keys(totals.models).length > 0;
      emit({
        type: 'turn_end',
        is_error: true,
        result: 'interrupted by the user',
        cost_usd: hasModels ? (totals.modelCost ?? 0) : totals.cost ?? null,
        duration_ms: Date.now() - startedAt,
        ...(hasModels ? { model_usage: totals.models } : {}),
      });
      return { sessionId: null, failure: 'interrupted by the user' };
    }
    let text = '';
    let stderrTail = '';
    let failure = null;
    let endEvent = null;
    let errorEvent = null;
    const thoughts = [];
    // colonizer__* MCP calls are question traffic, not work: §2 says a question is never also a
    // tool_call/tool_result, so the call ids are remembered only to drop their updates.
    const colonizerCalls = new Set();
    child = spawnFn(grokBin(env), turnArgs({ promptFile, model, sessionId }), { env: childEnv(env, home), stdio: ['ignore', 'pipe', 'pipe'] });
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => (stderrTail = (stderrTail + chunk).slice(-2000)));

    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    lines.on('line', (line) => {
      if (!line.trim()) return;
      let event;
      try {
        event = JSON.parse(line);
      } catch {
        emit({ type: 'log', level: 'warn', message: `grok emitted a line that is not JSON: ${clip(line, 200)}` });
        return;
      }
      switch (event.type) {
        case 'text':
          if (typeof event.data === 'string' && event.data) {
            text += event.data;
            emit({ type: 'assistant_text_delta', message_id: messageId, block_index: 0, delta: event.data });
          }
          break;
        case 'thought':
          if (typeof event.data === 'string' && event.data) thoughts.push(event.data);
          break;
        case 'tool_call': {
          const name = String(event.toolName ?? event.kind ?? 'unknown');
          if (name.startsWith('colonizer__')) {
            if (event.toolCallId != null) colonizerCalls.add(String(event.toolCallId));
            break;
          }
          emit({ type: 'tool_call', message_id: messageId, tool_call_id: String(event.toolCallId ?? ''), name, input: plainObject(event.rawInput) });
          break;
        }
        case 'tool_call_update': {
          if (colonizerCalls.has(String(event.toolCallId ?? ''))) break; // its outcome is the question_answered flow
          if (PROGRESS.has(String(event.status ?? ''))) break; // progress only; the result follows
          emit({ type: 'tool_result', tool_call_id: String(event.toolCallId ?? ''), output: clip(toolOutput(event)), is_error: /fail|error|denied|cancel/i.test(String(event.status ?? '')) });
          break;
        }
        case 'end':
          endEvent = event;
          mergeUsage(totals, event);
          break;
        case 'error':
          errorEvent = event;
          mergeUsage(totals, event);
          break;
        case 'usage': // per-response boundary; the end event carries the turn's spend
        case 'plan':
        case 'available_commands': // known grok events with no protocol counterpart
          break;
        default:
          // The event list is explicitly non-exhaustive upstream; unknown types must not kill a turn.
          emit({ type: 'log', level: 'warn', message: `ignored an unknown grok streaming event type: ${JSON.stringify(event.type)}` });
      }
    });

    const [code, signalName] = await once(child, 'close');
    rmSync(promptFile, { force: true });
    if (errorEvent) failure = `grok error: ${errorEvent.message ?? 'unknown error'}`;
    // An interrupted turn is truncated mid-stream even when grok still emits `end`; the partial
    // result must not look like success to a UI that would publish it.
    else if (interrupted) failure = 'interrupted by the user';
    else if (!endEvent) {
      failure =
        `grok exited without an end event (exit code ${code ?? 'null'}${signalName ? `, signal ${signalName}` : ''}) — the ` +
        `streaming-json framing drifted, or grok crashed; stderr tail: ${clip(stderrTail.trim(), 500) || '(empty)'}`;
    }

    if (thoughts.length) emit({ type: 'thinking', message_id: messageId, block_index: 1, text: thoughts.join('\n') });
    if (text) emit({ type: 'assistant_text', message_id: messageId, block_index: 0, text });
    const hasModels = Object.keys(totals.models).length > 0;
    emit({
      type: 'turn_end',
      is_error: Boolean(failure),
      result: failure ?? (text || null),
      cost_usd: hasModels ? (totals.modelCost ?? 0) : totals.cost ?? null,
      duration_ms: Date.now() - startedAt,
      ...(hasModels ? { model_usage: totals.models } : {}),
    });
    return { sessionId: endEvent?.sessionId ?? null, failure };
  })();

  return {
    done,
    interrupt() {
      interrupted = true;
      if (!child || child.exitCode !== null || child.signalCode) return;
      child.kill('SIGINT');
      setTimeout(() => child?.kill('SIGKILL'), 2000).unref();
    },
  };
}

/** A push-only async queue: `next()` resolves per command, and `close()` ends the stream with null. */
class AsyncQueue {
  #pending = [];
  #waiting = [];
  #closed = false;

  push(value) {
    if (this.#closed) return;
    const waiter = this.#waiting.shift();
    if (waiter) waiter.resolve(value);
    else this.#pending.push(value);
  }

  close() {
    this.#closed = true;
    for (const waiter of this.#waiting.splice(0)) waiter.resolve(null);
  }

  next() {
    if (this.#pending.length) return Promise.resolve(this.#pending.shift());
    if (this.#closed) return Promise.resolve(null);
    return new Promise((resolve) => this.#waiting.push({ resolve }));
  }
}

/** The command loop: turns run one at a time (one prompt per grok process); a user_message that
 * arrives mid-turn is queued for the next slot, while interrupt, set_model and answer apply at once. */
export async function run({ commands, emit, env, spawnFn = spawn }) {
  let status = null;
  // One place emits statuses, so the bridge's waiting_for_answer/working flips stay deduped.
  const setStatus = (state, detail) => { if (state === status && detail === undefined) return; status = state; emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail }); };
  setStatus('idle');
  const problem = await preflight({ env, spawnFn });
  const home = mkdtempSync(join(tmpdir(), 'colonizer-grok-'));
  if (problem) {
    emit({ type: 'log', level: 'error', message: `${problem.code}: ${problem.message}` });
    setStatus('error', problem.code);
  }

  let modelSpec = String(env.COLONIZER_MODEL ?? '');
  let currentModel = null; // what the UI last heard through model_changed
  let sessionId = null;
  let turn = null;
  let pumping = false;
  const pending = [];
  const totals = { cost: undefined, models: {} };
  let n = 0;

  // The loopback bridge mcp.mjs talks to; grok finds its URL and token in the config written next.
  const bridge = await createBridge({ emit, setStatus, isWorking: () => turn !== null });
  writeFileSync(join(home, 'config.toml'), `${homeConfig({ bridge })}\n`);

  const pump = async () => {
    if (pumping) return;
    pumping = true;
    try {
      while (pending.length) {
        const message = pending.shift();
        setStatus('working');
        const resolved = resolveModel(modelSpec);
        if (problem || resolved.error) {
          const result = problem ? `${problem.code}: ${problem.message}` : resolved.error;
          emit({ type: 'turn_end', is_error: true, result, cost_usd: null, duration_ms: 0 });
          if (problem) setStatus('error', problem.code);
          else setStatus('idle');
          continue;
        }
        if (currentModel === null && resolved.model) {
          currentModel = resolved.model;
          emit({ type: 'model_changed', model: resolved.model, previous: null });
        }
        n += 1;
        turn = startTurn({ prompt: message.text, model: resolved.model, sessionId, messageId: `msg-${n}`, env, home, emit, spawnFn, totals });
        try {
          const result = await turn.done;
          sessionId = result.sessionId ?? sessionId;
        } catch (err) {
          // A turn that throws (a prompt file that cannot be written, say) must still end as an
          // error turn, or the colony's turn never terminates.
          turn = null;
          bridge.cancelAll(); // a dead turn cannot answer its open asks any more
          emit({ type: 'log', level: 'error', message: `the turn crashed: ${err?.message ?? err}` });
          emit({ type: 'turn_end', is_error: true, result: `the turn crashed: ${err?.message ?? err}`, cost_usd: null, duration_ms: 0 });
          setStatus('error', 'turn crashed');
          break;
        }
        turn = null;
        bridge.cancelAll(); // a turn that merely ended leaves its open asks stale too
        setStatus(bridge.pending() ? 'waiting_for_answer' : 'idle');
      }
    } finally {
      pumping = false;
    }
  };

  for (;;) {
    const command = await commands.next();
    if (!command || command.type === 'shutdown') break; // shutdown or stdin EOF
    switch (command.type) {
      case 'user_message': {
        if (typeof command.text !== 'string' || !command.text.trim()) {
          emit({ type: 'log', level: 'warn', message: 'ignored a user_message without text' });
          break;
        }
        emit({ type: 'user_message', id: command.id, text: command.text });
        pending.push(command);
        pump();
        break;
      }
      case 'interrupt':
        bridge.cancelAll(); // the interrupted turn cannot answer its open asks any more
        turn?.interrupt(); // the turn ends as an error naming the interrupt; the runner stays up
        break;
      case 'set_model': {
        const resolved = resolveModel(command.model);
        if (resolved.error) {
          emit({ type: 'log', level: 'error', message: resolved.error });
          break;
        }
        if (!resolved.model) {
          emit({ type: 'log', level: 'warn', message: 'ignored a set_model without a model' });
          break;
        }
        modelSpec = String(command.model).trim();
        emit({ type: 'model_changed', model: resolved.model, previous: currentModel });
        currentModel = resolved.model; // applied from the next turn on: one grok process per turn
        break;
      }
      case 'answer': {
        const answers = command.answers && typeof command.answers === 'object' && !Array.isArray(command.answers) ? command.answers : {};
        const response = typeof command.response === 'string' && command.response.trim() ? command.response : null;
        if (!bridge.answer(command.question_id, answers, response)) emit({ type: 'log', level: 'warn', message: `no open question with id ${command.question_id}` });
        break;
      }
      default:
        break; // unknown commands are ignored (protocol forward compatibility)
    }
  }

  if (turn) {
    turn.interrupt();
    await Promise.race([turn.done, sleep(3000)]);
  }
  bridge.cancelAll();
  setStatus('exited');
  await bridge.close();
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

  await run({ commands, emit, env: process.env });
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
