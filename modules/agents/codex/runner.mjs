#!/usr/bin/env node
// Colonizer agent runner for OpenAI's Codex CLI (`codex`), headless: the runner contract of
// docs/protocol.md §2 (JSON-line commands on stdin, JSON-line protocol events on stdout). One
// `codex exec` child per turn, the prompt on stdin; the first turn's `thread.started` event carries
// the codex thread id and every later turn resumes it with `resume`, so a colony is one continuous
// codex thread. The model asks the user through the colonizer MCP server (mcp.mjs, registered in
// the fresh CODEX_HOME's config.toml); its asks park on this runner's loopback bridge until the
// matching `answer` command. The pin lives in module.json; every flag and event field is cited in
// the README.

import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtempSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

// The named preflight problems (README, "Preflight"): each is the detail on the `status error`
// event and the prefix of the log that says how to fix it.
export const MISSING_CREDENTIAL = 'CODEX_CREDENTIAL_MISSING';
export const MISSING_BINARY = 'CODEX_BINARY_MISSING';
export const VERSION_DRIFT = 'CODEX_VERSION_DRIFT';
export const MODEL_PROVIDER = 'CODEX_MODEL_PROVIDER';

/** The pin, read from module.json so the manifest and this preflight cannot drift apart. */
export function readPin() {
  const pin = JSON.parse(readFileSync(join(here, 'module.json'), 'utf8'))?.requires?.pins?.codex;
  if (!pin?.version) throw new Error('module.json carries no codex pin');
  return pin;
}

/** Where the codex binary is: COLONIZER_CODEX_BIN wins, else `codex` on the PATH. */
export function codexBin(env) {
  return String(env.COLONIZER_CODEX_BIN ?? '').trim() || 'codex';
}

/** The first X.Y.Z in `codex --version`'s output ("codex-cli 0.156.1"), whatever else surrounds it. */
export function parseVersion(text) {
  const m = /\b(\d+)\.(\d+)\.(\d+)\b/.exec(String(text ?? ''));
  return m ? `${m[1]}.${m[2]}.${m[3]}` : null;
}

/** `codex --version`'s output: undefined when the binary could not run, null when it ran and failed. */
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

/** The checks that must pass before any codex process is spawned: fail loudly, not on a prompt. */
export async function preflight({ env, spawnFn = spawn, pin = readPin() }) {
  // Either secret name is a credential, and an empty one counts as absent — the same rule the
  // error below states, and the same rule childEnv applies when it names the key for the child.
  const credential = String(env.CODEX_API_KEY ?? '').trim() || String(env.OPENAI_API_KEY ?? '').trim();
  if (!credential) {
    return {
      code: MISSING_CREDENTIAL,
      message:
        'CODEX_API_KEY is unset or empty, and the colony never runs browser OAuth (codex login). Add an OpenAI API key ' +
        'from platform.openai.com as a colony secret named CODEX_API_KEY for host api.openai.com, so the mothership ' +
        'injects it into this colony (README, "Credential story").',
    };
  }
  const bin = codexBin(env);
  const text = await versionOf(bin, spawnFn);
  const install = `npm install -g @openai/codex@${pin.version}`;
  if (text === undefined) {
    return { code: MISSING_BINARY, message: `the codex CLI was not found at "${bin}". Install the pinned version: ${install}` };
  }
  const found = parseVersion(text);
  if (found !== pin.version) {
    return {
      code: VERSION_DRIFT,
      message: `codex --version printed "${String(text).trim()}" (parsed ${found ?? 'nothing'}) instead of the pinned ` +
        `${pin.version} (SOURCE_REV ${pin.source_rev}). Install the pinned version: ${install}`,
    };
  }
  return null;
}

/** The `-m` value for a model setting: `openai/<model>` or a bare OpenAI model id; any other
 * provider prefix is refused by name (gateway routing is a follow-up, README "Credential story").
 * An empty setting means no `-m`: codex then runs on the CLI's own default model. */
export function resolveModel(spec) {
  const value = String(spec ?? '').trim();
  if (!value) return {};
  const slash = value.indexOf('/');
  if (slash > 0 && value.slice(0, slash) !== 'openai') {
    return { error: `${MODEL_PROVIDER}: "${value}" names another provider; this module runs openai/<model> only` };
  }
  return { model: slash > 0 ? value.slice(slash + 1) : value };
}

/** One headless codex turn (`codex exec --json`): the prompt rides stdin (`-`), exec-level options
 * come before the `resume` subcommand (which takes no `-c`), and the nesting decisions from the
 * README applied. */
export function turnArgs({ model, threadId }) {
  const args = [
    '--json', // events as JSONL on stdout (developers.openai.com/codex/noninteractive)
    '--skip-git-repo-check', // the runner may sit anywhere; the colony VM is the boundary
    '--dangerously-bypass-approvals-and-sandbox', // codex's own landlock/seccomp stays off; the microVM is the boundary
    '-c', 'check_for_update_on_startup=false', // no update checks inside a colony (upstream config reference)
    '-c', 'history.persistence="none"', // no prompt history file; the session rollout stays (resume needs it)
    '-c', 'otel.metrics_exporter="none"', // product analytics off (upstream config reference)
  ];
  if (model) args.push('-m', model);
  if (threadId) args.push('resume', threadId); // resume: a colony is one continuous codex thread
  args.push('-'); // the prompt: read from stdin
  return args;
}

/** The child's environment. A fresh CODEX_HOME is the one nesting lever the docs verify: codex's
 * config.toml, auth.json and session rollouts all live under it (README "Nesting"). The API key
 * itself rides the inherited environment: `codex exec` reads CODEX_API_KEY from it — and only that
 * name, so an OPENAI_API_KEY credential the preflight accepted is handed down under the real name. */
export function childEnv(env, home) {
  const mapped = { ...env };
  if (!String(mapped.CODEX_API_KEY ?? '').trim() && String(mapped.OPENAI_API_KEY ?? '').trim()) {
    mapped.CODEX_API_KEY = mapped.OPENAI_API_KEY;
  }
  return {
    ...mapped,
    BROWSER: '/bin/false', // belt-and-braces: nothing may open a browser, and this runner never runs codex login
    CODEX_HOME: home,
  };
}

/** The fresh CODEX_HOME's config.toml: the colonizer MCP server whose single `ask_user` tool is
 * how the model reaches the user (README, "Questions"). config.toml rather than `-c` overrides so
 * the bridge token is not on argv; `tool_timeout_sec` must outlive any human answer, and
 * `default_tools_approval_mode = "approve"` auto-approves the call (as the bypass flag already
 * does for MCP). JSON.stringify of a string is a valid TOML basic string for our paths and URLs. */
export function codexConfig({ node = process.execPath, moduleDir = here, bridge }) {
  const s = (value) => JSON.stringify(value);
  return [
    '[mcp_servers.colonizer]',
    `command = ${s(node)}`,
    `args = [${s(join(moduleDir, 'mcp.mjs'))}]`,
    `tool_timeout_sec = ${MCP_TOOL_TIMEOUT_SECS}`,
    'default_tools_approval_mode = "approve"',
    '',
    '[mcp_servers.colonizer.env]',
    `COLONIZER_BRIDGE_URL = ${s(bridge.url)}`,
    `COLONIZER_BRIDGE_TOKEN = ${s(bridge.token)}`,
    '',
  ].join('\n');
}

// docs/protocol.md §2 caps tool_result output at 20 000 characters.
const TOOL_RESULT_LIMIT = 20000;
// config.toml's mcp tool_timeout_sec: codex defaults a tool call to a few minutes, but an ask
// waits on a human (README, "Questions").
const MCP_TOOL_TIMEOUT_SECS = 3600;
// item types that are a tool: started emits the tool_call, completed its tool_result. agent_message
// and reasoning are the model's own voice; todo_list (plan updates) has no protocol counterpart.
const TOOL_ITEMS = new Set(['command_execution', 'file_change', 'mcp_tool_call', 'web_search']);
// colonizer-server MCP calls are question traffic, not work: §2 says a question is never also a
// tool_call/tool_result, so those items are dropped (the bridge's question events carry them).
const isAsk = (item) => item.type === 'mcp_tool_call' && item.server === 'colonizer';

const clip = (text, limit = TOOL_RESULT_LIMIT) => (text.length > limit ? `${text.slice(0, limit - 1)}…` : text);
const plainObject = (value) => (value && typeof value === 'object' && !Array.isArray(value) ? value : {});
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** A tool item's protocol input and result text, the fields named as codex names them. */
const toolInput = (item) =>
  plainObject({ command: item.command, changes: item.changes, server: item.server, tool: item.tool, query: item.query });
const toolOutput = (item) => {
  const rest = { ...item };
  for (const key of ['id', 'type', 'status']) delete rest[key];
  const output = item.aggregated_output ?? rest;
  const parts = [typeof output === 'string' ? output : JSON.stringify(output)];
  if (Number.isFinite(item.exit_code) && item.exit_code !== 0) parts.push(`exit code ${item.exit_code}`);
  return parts.join('\n');
};
/** A tool item's outcome: codex marks failures with a status word or a non-zero exit code. */
const toolIsError = (item) => /fail|error|declin/i.test(String(item.status ?? '')) || (Number.isFinite(item.exit_code) && item.exit_code !== 0);

/** codex's `turn.completed` spend, accumulated across turns into the colony-cumulative totals
 * turn_end wants (§2): one codex process per turn means each event carries only that turn's spend.
 * codex names no model on the stream, so the resolved model (or the CLI's default) is the key. */
export function mergeUsage(totals, model, usage) {
  if (!usage || typeof usage !== 'object') return;
  const soFar = (totals.models[model || 'codex'] ??= { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 });
  soFar.input_tokens += usage.input_tokens ?? 0;
  soFar.output_tokens += usage.output_tokens ?? 0;
  soFar.cache_read_tokens += usage.cached_input_tokens ?? 0;
}

/** Runs one turn as a codex child, emitting the mapped protocol events as they arrive; resolves with
 * the turn's codex thread id once the child exits. `interrupt()` SIGINTs the child (codex saves the
 * session rollout continuously) and escalates to SIGKILL after a grace period. */
export function startTurn({ prompt, model, threadId, messageId, env, home, emit, spawnFn = spawn, totals }) {
  let child = null;
  let interrupted = false;
  // The thread id to carry into the next turn: whatever `thread.started` named last, else the one
  // this turn resumed.
  let latestThread = threadId ?? null;
  const done = (async () => {
    const startedAt = Date.now();
    const text = [];
    const thoughts = [];
    let stderrTail = '';
    let completed = null;
    let failed = null;
    let failure = null;
    child = spawnFn(codexBin(env), turnArgs({ model, threadId }), { env: childEnv(env, home), stdio: ['pipe', 'pipe', 'pipe'] });
    // The prompt rides stdin (`-` as the prompt argument): an issue brief can be far larger than
    // an argv slot, and a child that exits early must not turn a broken pipe into a crash.
    child.stdin.on('error', () => {});
    child.stdin.end(prompt);
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => (stderrTail = (stderrTail + chunk).slice(-2000)));

    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    lines.on('line', (line) => {
      if (!line.trim()) return;
      let event;
      try {
        event = JSON.parse(line);
      } catch {
        emit({ type: 'log', level: 'warn', message: `codex emitted a line that is not JSON: ${clip(line, 200)}` });
        return;
      }
      switch (event.type) {
        case 'thread.started':
          if (typeof event.thread_id === 'string' && event.thread_id) latestThread = event.thread_id;
          break;
        case 'item.started':
          if (TOOL_ITEMS.has(event.item?.type) && !isAsk(event.item)) emit({ type: 'tool_call', message_id: messageId, tool_call_id: String(event.item.id ?? ''), name: event.item.type, input: toolInput(event.item) });
          break;
        case 'item.updated': // progress only; the completed item carries the outcome
          break;
        case 'item.completed': {
          const item = plainObject(event.item);
          if (item.type === 'agent_message' && typeof item.text === 'string' && item.text) text.push(item.text);
          else if (item.type === 'reasoning' && typeof item.text === 'string' && item.text) thoughts.push(item.text);
          else if (TOOL_ITEMS.has(item.type) && !isAsk(item)) {
            emit({ type: 'tool_result', tool_call_id: String(item.id ?? ''), output: clip(toolOutput(item)), is_error: toolIsError(item) });
          }
          break;
        }
        case 'turn.completed':
          completed = event;
          mergeUsage(totals, model, event.usage);
          break;
        case 'turn.failed':
          failed = event;
          break;
        case 'error':
          // Transient by nature upstream (reconnect notices; the fatal ones end in turn.failed):
          // logged, never fatal on their own.
          emit({ type: 'log', level: 'warn', message: `codex error event: ${clip(String(event.message ?? 'unknown'), 500)}` });
          break;
        case 'turn.started': // known codex events with no protocol counterpart
        case 'todo_list':
          break;
        default:
          // The event list is explicitly non-exhaustive upstream; unknown types must not kill a turn.
          emit({ type: 'log', level: 'warn', message: `ignored an unknown codex event type: ${JSON.stringify(event.type)}` });
      }
    });

    const [code, signalName] = await once(child, 'close');
    if (failed) failure = `codex turn failed: ${failed.error?.message ?? 'unknown error'}`;
    // An interrupted turn is truncated mid-stream even when codex still reports a completion; the
    // partial result must not look like success to a UI that would publish it.
    else if (interrupted) failure = 'interrupted by the user';
    else if (!completed) {
      failure =
        `codex exited without a turn.completed event (exit code ${code ?? 'null'}${signalName ? `, signal ${signalName}` : ''}) — the ` +
        `--json framing drifted, or codex crashed; stderr tail: ${clip(stderrTail.trim(), 500) || '(empty)'}`;
    }

    if (thoughts.length) emit({ type: 'thinking', message_id: messageId, block_index: 1, text: thoughts.join('\n') });
    if (text.length) emit({ type: 'assistant_text', message_id: messageId, block_index: 0, text: text.join('\n\n') });
    const hasModels = Object.keys(totals.models).length > 0;
    emit({
      type: 'turn_end',
      is_error: Boolean(failure),
      result: failure ?? (text.length ? text.join('\n\n') : null),
      cost_usd: null, // codex reports tokens, never cost; like the hermes runner, cost stays unset
      duration_ms: Date.now() - startedAt,
      ...(hasModels ? { model_usage: totals.models } : {}),
    });
    return { threadId: latestThread, failure };
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

/** Loopback HTTP bridge to mcp.mjs: an ask_user call parks here until the matching `answer`
 * command arrives on stdin (docs/protocol.md §2's question card). Shaped like the other runners'
 * bridges; this one serves `/ask` only. */
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

/** The command loop: turns run one at a time (one prompt per codex process); a user_message that
 * arrives mid-turn is queued for the next slot, while interrupt, set_model and answer apply at once. */
export async function run({ commands, emit, env, spawnFn = spawn }) {
  let status = null;
  // One place emits statuses, so the bridge's waiting_for_answer/working flips stay deduped.
  const setStatus = (state, detail) => { if (state === status && detail === undefined) return; status = state; emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail }); };
  setStatus('idle');
  const problem = await preflight({ env, spawnFn });
  const home = mkdtempSync(join(tmpdir(), 'colonizer-codex-'));

  let modelSpec = String(env.COLONIZER_MODEL ?? '');
  let currentModel = null; // what the UI last heard through model_changed
  let threadId = null;
  let turn = null;
  let pumping = false;
  const pending = [];
  const totals = { models: {} };
  let n = 0;
  const bridge = await createBridge({ emit, setStatus, isWorking: () => turn !== null });
  writeFileSync(join(home, 'config.toml'), codexConfig({ bridge }));
  if (problem) {
    emit({ type: 'log', level: 'error', message: `${problem.code}: ${problem.message}` });
    setStatus('error', problem.code);
  }

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
        turn = startTurn({ prompt: message.text, model: resolved.model, threadId, messageId: `msg-${n}`, env, home, emit, spawnFn, totals });
        try {
          const result = await turn.done;
          threadId = result.threadId ?? threadId;
        } catch (err) {
          // A turn that throws must still end as an error turn, or the colony's turn never terminates.
          emit({ type: 'log', level: 'error', message: `the turn crashed: ${err?.message ?? err}` });
          emit({ type: 'turn_end', is_error: true, result: `the turn crashed: ${err?.message ?? err}`, cost_usd: null, duration_ms: 0 });
          setStatus('error', 'turn crashed');
          break;
        } finally {
          turn = null;
          bridge.cancelAll(); // the child is gone: an ask it left parked is released as cancelled
        }
        setStatus('idle');
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
        if (resolved.error || !resolved.model) {
          emit({ type: 'log', level: resolved.error ? 'error' : 'warn', message: resolved.error ?? 'ignored a set_model without a model' });
          break;
        }
        modelSpec = String(command.model).trim();
        emit({ type: 'model_changed', model: resolved.model, previous: currentModel });
        currentModel = resolved.model; // applied from the next turn on: one codex process per turn
        break;
      }
      case 'answer': {
        // The model asked through ask_user and the bridge parked its HTTP response on this id.
        const answers = command.answers && typeof command.answers === 'object' && !Array.isArray(command.answers) ? command.answers : {};
        const response = typeof command.response === 'string' && command.response.trim() ? command.response : null;
        if (!bridge.answer(command.question_id, answers, response)) emit({ type: 'log', level: 'warn', message: `no open question with id ${command.question_id}` });
        break;
      }
      default:
        break; // unknown commands are ignored (protocol forward compatibility)
    }
  }

  bridge.cancelAll(); // a parked ask is released with the turn above; the orphaned mcp.mjs drains
  if (turn) {
    turn.interrupt();
    await Promise.race([turn.done, sleep(3000)]);
  }
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
