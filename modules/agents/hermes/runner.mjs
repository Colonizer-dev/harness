#!/usr/bin/env node
// Colonizer agent runner for the Hermes Agent CLI (Nous Research), verified against hermes-agent
// v2026.9.24 (v0.21.5). Implements the runner contract in docs/protocol.md §2: commands arrive as
// JSON lines on stdin, protocol events leave as JSON lines on stdout, diagnostics go to stderr.
// One `hermes chat -q <text> --format stream-json` process per turn, later turns --resume the
// session id the first one reported.

import { execFile, spawn } from 'node:child_process';
import { mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

export const DEFAULT_TURN_TIMEOUT_SECS = 3600;
export const DEFAULT_HERMES_HOME = '/tmp/colonizer-hermes';
export const SESSION_FILE = 'colonizer-session-id';
export const DISABLED_TOOLSETS = ['memory', 'skills', 'delegation', 'cronjob', 'tts', 'clarify'];
export const MAX_TOOL_OUTPUT = 20_000;
// Hermes falls back to local silently when another terminal backend is unusable, so the runner pins
// local and refuses to start on anything else.
const LOCAL_BACKEND = 'local';
const HEADER_NAME = /^[A-Za-z0-9-]+$/;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Minimal async queue usable as an AsyncIterable (stdin commands), as the claude-code runner does. */
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

/** COLONIZER_MODEL_ROUTES → validated routes, the same wire shape the claude-code module reads (§6.1). */
export function parseRoutes(raw) {
  let data;
  try {
    data = JSON.parse(raw ?? '');
  } catch {
    return { routes: [], warnings: ['ignoring COLONIZER_MODEL_ROUTES: not valid JSON'] };
  }
  if (!Array.isArray(data)) return { routes: [], warnings: ['ignoring COLONIZER_MODEL_ROUTES: expected a JSON array'] };
  const routes = [];
  const warnings = [];
  data.forEach((entry, i) => {
    const prefix = typeof entry?.prefix === 'string' ? entry.prefix : '';
    const baseUrl = typeof entry?.base_url === 'string' ? entry.base_url : '';
    if (!prefix.endsWith('/') || prefix.length < 2 || !/^https?:\/\//.test(baseUrl)) {
      warnings.push(`ignoring model route ${i}: needs a "<provider>/" prefix and an http(s) base_url`);
      return;
    }
    const rawHeaders = entry.headers && typeof entry.headers === 'object' && !Array.isArray(entry.headers) ? entry.headers : {};
    const headers = Object.fromEntries(
      Object.entries(rawHeaders)
        .filter(([name, header]) => HEADER_NAME.test(name) && typeof header === 'string')
        .map(([name, header]) => [name.toLowerCase(), header]),
    );
    routes.push({
      provider: typeof entry.provider === 'string' && entry.provider ? entry.provider : prefix.slice(0, -1),
      prefix,
      base_url: baseUrl,
      headers,
    });
  });
  return { routes, warnings };
}

/**
 * $HERMES_HOME/config.yaml, written as JSON (JSON is valid YAML, so no YAML dependency): the local
 * terminal backend, memory and background review off, one provider per gateway route speaking the
 * Anthropic Messages wire, and the `model:` block naming the resolved pair. That block is load-bearing:
 * Hermes' first-run guard (`_has_any_provider_configured()`) ignores the top-level `providers:` map
 * and exits with "no API keys or providers found" unless `model.provider` points at one.
 */
export function hermesConfig(routes, resolved = null) {
  const providers = {};
  for (const route of routes) {
    providers[`colonizer-${route.provider}`] = { api: route.base_url, transport: 'anthropic_messages', extra_headers: route.headers };
  }
  const config = {
    terminal: { backend: LOCAL_BACKEND },
    memory: { memory_enabled: false, user_profile_enabled: false },
    skills: { write_approval: true },
    auxiliary: { background_review: { enabled: false } },
    agent: { disabled_toolsets: [...DISABLED_TOOLSETS] },
    providers,
  };
  if (resolved) config.model = { provider: resolved.provider, default: resolved.model };
  return config;
}

/**
 * `<provider>/<model>` → the flags Hermes needs. The route prefix carries the provider id and the
 * gateway expects the bare model (the claude-code router strips the same prefix before forwarding),
 * so the remainder after the prefix is what `-m` gets. Everything else is refused, Portal included.
 */
export function resolveModel(model, routes) {
  const value = typeof model === 'string' ? model.trim() : '';
  if (!value) {
    return { ok: false, error: 'no model configured: set the agent model setting to <provider>/<model> — headless Hermes has no default to fall back on' };
  }
  if (value.startsWith('nous/')) {
    return { ok: false, error: `Nous Portal model ${value} is refused: Portal credits are a prepaid pool the provider gateway cannot account for (issue #199)` };
  }
  const route = routes.find((r) => value.startsWith(r.prefix));
  if (!route) {
    return {
      ok: false,
      error: value.includes('/')
        ? `model ${value} names a provider with no gateway route; add that provider in Settings first`
        : `model ${value} has no <provider>/ prefix; headless Hermes runs only on gateway-routed providers`,
    };
  }
  return { ok: true, provider: `colonizer-${route.provider}`, model: value.slice(route.prefix.length), full: value };
}

/** Names any terminal backend other than local the inherited TERMINAL_ENV asks for, or null when safe. */
export function backendRefusal(env = process.env) {
  const named = (env.TERMINAL_ENV ?? '').trim();
  if (!named || named === LOCAL_BACKEND) return null;
  return `TERMINAL_ENV=${named} names a terminal backend other than "local"; Hermes would fall back to local silently when that backend is unusable, so this runner refuses to start instead. Only "local" is supported inside a colony microVM.`;
}

/** Runs `<bin> --version`; anything but a clean exit is a failed probe. */
export function probeHermes({ bin = ['hermes'], execFileImpl = execFile, timeoutMs = 10_000 } = {}) {
  return new Promise((resolve) => {
    execFileImpl(bin[0], [...bin.slice(1), '--version'], { encoding: 'utf8', timeout: timeoutMs }, (error, stdout) => {
      if (error) {
        const reason = error.code === 'ENOENT' ? 'not found on PATH' : `failed with ${error.code ?? error.message}`;
        resolve({ ok: false, error: `hermes binary \`${bin.join(' ')}\` ${reason}` });
      } else {
        resolve({ ok: true, version: String(stdout).trim() });
      }
    });
  });
}

const truncate = (text) => {
  if (text.length <= MAX_TOOL_OUTPUT) return text;
  const suffix = `\n… [truncated ${text.length - MAX_TOOL_OUTPUT} characters]`;
  return text.slice(0, MAX_TOOL_OUTPUT - suffix.length) + suffix;
};

/**
 * Drives Hermes over the runner protocol.
 * @param {object} args
 * @param {string[]} [args.hermes]               the hermes command, injectable so tests use the stub
 * @param {AsyncIterable<object>} args.commands  parsed stdin commands
 * @param {Function} args.emit                   writes one protocol event
 * @param {object} [args.env]                    the runner environment (routes, model, timeout)
 * @param {string} [args.home]                   HERMES_HOME, created and configured here
 * @param {Function} [args.spawnImpl]            child_process.spawn for the hermes turns
 */
export async function runAgent({ hermes = ['hermes'], commands, emit, env = process.env, home = env.COLONIZER_HERMES_HOME || DEFAULT_HERMES_HOME, spawnImpl = spawn } = {}) {
  let status = null;
  const setStatus = (state, detail) => {
    if (state === status) return;
    status = state;
    emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail });
  };

  const { routes, warnings } = parseRoutes(env.COLONIZER_MODEL_ROUTES);
  for (const message of warnings) emit({ type: 'log', level: 'warn', message });
  mkdirSync(home, { recursive: true });
  const timeoutSecs = Number(env.COLONIZER_HERMES_TURN_TIMEOUT_SECS) || DEFAULT_TURN_TIMEOUT_SECS;
  let model = env.COLONIZER_MODEL ?? '';
  // Rewritten before every turn, so the file always names the model the CLI flags carry.
  const writeConfig = (resolved) => writeFileSync(join(home, 'config.yaml'), `${JSON.stringify(hermesConfig(routes, resolved), null, 2)}\n`);
  const startup = resolveModel(model, routes);
  writeConfig(startup.ok ? startup : null);
  let announced = null; // the model clients were told about, in <provider>/<model> form
  let sessionId = null;
  try {
    sessionId = readFileSync(join(home, SESSION_FILE), 'utf8').trim() || null;
  } catch {
    // a fresh home resumes nothing
  }
  const childEnv = { ...env, HERMES_HOME: home, TERMINAL_ENV: LOCAL_BACKEND };
  const modelUsage = {}; // full model name → cumulative tokens (turn_end.model_usage)
  let turnCount = 0;
  let child = null;
  let markKilled = () => {};
  let turnChain = Promise.resolve();
  const queued = [];
  let closing = false;

  /** SIGTERM is the only signal that stops a Hermes turn, so every stop goes through here. */
  const killChild = () => {
    if (!child) return;
    markKilled();
    child.kill('SIGTERM');
  };

  /** Announces the model once it is known routable, the way claude-code's init announcement does. */
  const announce = () => {
    if (announced === model) return;
    const previous = announced;
    announced = model;
    emit({ type: 'model_changed', model, previous });
  };

  async function turn(text) {
    const resolved = resolveModel(model, routes);
    if (!resolved.ok) {
      emit({ type: 'log', level: 'error', message: `refusing the turn: ${resolved.error}` });
      setStatus('error', resolved.error);
      // Every echoed user_message needs a terminal event, refused turn included.
      emit({ type: 'turn_end', is_error: true, result: null, cost_usd: null, duration_ms: null });
      return;
    }
    writeConfig(resolved);
    announce();
    turnCount += 1;
    const messageId = `hermes-${turnCount}`;
    const args = [...hermes.slice(1), 'chat', '-q', text, '--format', 'stream-json', '--provider', resolved.provider, '-m', resolved.model];
    if (sessionId) args.push('--resume', sessionId);
    setStatus('working');
    const startedAt = Date.now();
    const pending = new Map(); // tool name → FIFO queue of this turn's tool_call ids
    let toolCount = 0;
    let finished = false;
    let killed = false;
    markKilled = () => {
      killed = true;
    };

    const finish = ({ isError = false, result = null, durationMs = null, tokens = null } = {}) => {
      if (finished) return;
      finished = true;
      if (tokens) {
        const usage = (modelUsage[resolved.full] ??= { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 });
        for (const [theirs, ours] of [['input', 'input_tokens'], ['output', 'output_tokens'], ['cache_read', 'cache_read_tokens'], ['cache_write', 'cache_write_tokens']]) {
          if (Number.isFinite(tokens[theirs])) usage[ours] += tokens[theirs];
        }
      }
      if (result) emit({ type: 'assistant_text', message_id: messageId, block_index: 0, text: result });
      emit({
        type: 'turn_end',
        is_error: isError,
        result,
        // The gateway prices and budgets every routed request (§6.5), so the runner reports no cost.
        cost_usd: null,
        duration_ms: durationMs ?? Date.now() - startedAt,
        ...(Object.keys(modelUsage).length ? { model_usage: structuredClone(modelUsage) } : {}),
      });
      setStatus('idle');
    };

    child = spawnImpl(hermes[0], args, { cwd: process.cwd(), env: childEnv, stdio: ['ignore', 'pipe', 'pipe'] });
    // A spawn that cannot start (binary vanished mid-run) must end the turn, not hang it; the error
    // is logged here only — `killed` keeps the close path below from logging it a second time.
    child.on('error', (err) => {
      killed = true;
      emit({ type: 'log', level: 'error', message: `could not run hermes: ${err.message}` });
    });
    const timer = setTimeout(() => {
      emit({ type: 'log', level: 'error', message: `turn exceeded COLONIZER_HERMES_TURN_TIMEOUT_SECS (${timeoutSecs}s); terminating Hermes` });
      killChild();
    }, timeoutSecs * 1000);

    const lines = child.stdout ? createInterface({ input: child.stdout, crlfDelay: Infinity }) : null;
    lines?.on('line', (line) => {
      if (!line.trim()) return;
      let event;
      try {
        event = JSON.parse(line);
      } catch {
        emit({ type: 'log', level: 'warn', message: `hermes stdout (not JSON): ${line.slice(0, 500)}` });
        return;
      }
      switch (event?.type) {
        case 'system':
          if (event.subtype === 'init' && typeof event.session_id === 'string' && event.session_id) {
            sessionId = event.session_id;
            emit({ type: 'log', level: 'info', message: `Hermes session ${sessionId} started (model ${event.model ?? 'unknown'})` });
          }
          break;
        case 'text':
          if (event.text) emit({ type: 'assistant_text_delta', message_id: messageId, block_index: 0, delta: event.text });
          break;
        case 'tool_use': {
          const toolCallId = `hermes-tool-${turnCount}-${(toolCount += 1)}`;
          const queue = pending.get(event.name) ?? [];
          queue.push(toolCallId);
          pending.set(event.name, queue);
          emit({ type: 'tool_call', message_id: messageId, tool_call_id: toolCallId, name: String(event.name ?? ''), input: event.input ?? {} });
          break;
        }
        case 'tool_result': {
          // Hermes names the tool rather than the call, so a result pairs with the oldest unmatched call of that name.
          const paired = pending.get(event.name);
          const toolCallId = paired?.length ? paired.shift() : `hermes-tool-${turnCount}-${(toolCount += 1)}`;
          if (paired && !paired.length) pending.delete(event.name);
          const output = typeof event.output === 'string' ? event.output : JSON.stringify(event.output ?? '');
          emit({ type: 'tool_result', tool_call_id: toolCallId, output: truncate(output), is_error: Boolean(event.is_error) });
          break;
        }
        case 'result':
          if (typeof event.session_id === 'string' && event.session_id) sessionId = event.session_id;
          try {
            writeFileSync(join(home, SESSION_FILE), `${sessionId ?? ''}\n`);
          } catch {
            // a restarted runner then resumes nothing; not worth failing a finished turn over
          }
          if (event.error) emit({ type: 'log', level: 'error', message: `hermes turn failed: ${event.error}` });
          finish({
            isError: Boolean(event.error) || event.exit_code !== 0,
            result: typeof event.text === 'string' && event.text ? event.text : null,
            durationMs: typeof event.duration_ms === 'number' ? event.duration_ms : null,
            tokens: event.tokens,
          });
          break;
        default:
          break; // unknown Hermes events are ignored (forward compatibility)
      }
    });
    child.stderr?.on('data', (data) => process.stderr.write(data));
    const code = await new Promise((resolve) => {
      child.on('close', (exitCode) => resolve(exitCode));
      child.on('error', () => resolve(null));
    });
    clearTimeout(timer);
    child = null;
    markKilled = () => {};
    if (!finished) {
      // SIGTERM leaves no result event (exit 143), so an interrupt or timeout ends the turn here.
      if (!killed) emit({ type: 'log', level: 'error', message: `hermes exited with code ${code ?? 'null'} before a result event` });
      finish({ isError: true });
    }
  }

  const drain = () => {
    turnChain = turnChain
      .then(async () => {
        while (queued.length && !closing) await turn(queued.shift());
      })
      .catch((err) => {
        // A turn that throws must end as a protocol error, never an unhandled rejection.
        const message = String(err?.message ?? err);
        emit({ type: 'log', level: 'error', message });
        setStatus('error', message);
      });
  };

  setStatus('idle');
  commandLoop: for await (const command of commands) {
    switch (command?.type) {
      case 'user_message': {
        const text = typeof command.text === 'string' ? command.text : '';
        if (!text.trim()) {
          emit({ type: 'log', level: 'warn', message: 'ignored an empty user_message' });
          break;
        }
        const id = typeof command.id === 'string' && command.id ? command.id : `u-${queued.length + 1}`;
        emit({ type: 'user_message', id, text });
        queued.push(text);
        drain();
        break;
      }
      case 'answer':
        emit({ type: 'log', level: 'warn', message: 'headless Hermes has no question channel (the clarify toolset is disabled), so there is no question to answer' });
        break;
      case 'interrupt':
        if (child) {
          emit({ type: 'log', level: 'info', message: 'interrupt: terminating the Hermes process (SIGINT does not cancel a Hermes turn)' });
          killChild();
        }
        break;
      case 'set_model': {
        const next = typeof command.model === 'string' ? command.model.trim() : '';
        const resolved = resolveModel(next, routes);
        if (!resolved.ok) {
          emit({ type: 'log', level: 'warn', message: `set_model refused: ${resolved.error}` });
          break;
        }
        model = next;
        announce();
        break;
      }
      case 'shutdown':
        break commandLoop;
      default:
        break; // unknown commands are ignored (protocol forward compatibility)
    }
  }

  // Shutdown or stdin EOF: finish fast and leave no orphan behind.
  closing = true;
  killChild();
  await Promise.race([turnChain, sleep(2000)]);
  if (child) {
    child.kill('SIGKILL');
    await Promise.race([new Promise((resolve) => child.on('close', resolve)), sleep(1000)]);
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

  const fail = (message) => {
    emit({ type: 'log', level: 'error', message });
    emit({ type: 'status', state: 'error', detail: message });
    process.stderr.write(`${message}\n`);
    process.exit(1);
  };
  const refuse = backendRefusal(process.env);
  if (refuse) fail(refuse);
  const bin = (process.env.COLONIZER_HERMES_BIN || 'hermes').split(' ').filter(Boolean);
  const probe = await probeHermes({ bin });
  if (!probe.ok) {
    fail(
      `${probe.error}. This module needs hermes-agent v2026.9.24 in the colony image: git clone --depth 1 --branch v2026.9.24 https://github.com/NousResearch/hermes-agent into a Python 3.11 venv, then pip install -e . Nothing stages that binary into the VM yet.`,
    );
  }
  emit({ type: 'log', level: 'info', message: `hermes probe: ${probe.version}; terminal backend local; disabled toolsets: ${DISABLED_TOOLSETS.join(', ')}` });

  await runAgent({ hermes: bin, commands, emit });
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
