#!/usr/bin/env node
// Colonizer agent runner for any Agent Client Protocol agent (Zed's ACP: JSON-RPC 2.0 over stdio,
// newline-delimited — https://agentclientprotocol.com), mapped onto the runner contract of
// docs/protocol.md §2 (JSON-line commands on stdin, protocol events on stdout). One long-lived ACP
// agent process per colony: `initialize` + `session/new` at boot (a resume boot `session/load`s the
// COLONIZER_RESUME_SESSION id instead, when the agent can reload it), every user_message one
// `session/prompt` turn. First verified agent is Google's Gemini CLI (`gemini --experimental-acp`);
// any other ACP agent runs through the custom-command setting (README).

import { spawn } from 'node:child_process';
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

import { evaluateExecPolicy, execPolicyLogLine, execPolicyReason, loadExecPolicy } from './execpolicy.mjs';
import { loadPathPolicy, matchPathPolicy } from './pathpolicy.mjs';

// Named preflight problems (README): the detail on the `status error` event, and the log prefix.
export const AGENT_UNKNOWN = 'ACP_AGENT_UNKNOWN';
export const CREDENTIAL_MISSING = 'ACP_CREDENTIAL_MISSING';
export const AGENT_FAILED = 'ACP_AGENT_FAILED';

// The known presets; `custom` takes its command line from the `command` setting instead.
const PRESETS = { gemini: { command: 'gemini --experimental-acp', credential: 'GEMINI_API_KEY' } };

/** The ACP agent argv: the preset's command, or COLONIZER_ACP_COMMAND for `custom`; null unknown. */
function agentArgv(env) {
  const preset = String(env.COLONIZER_ACP_AGENT ?? '').trim() || 'gemini';
  if (preset === 'custom') return { preset, argv: splitCommand(env.COLONIZER_ACP_COMMAND ?? '') };
  return { preset, argv: PRESETS[preset] ? splitCommand(PRESETS[preset].command) : null };
}

/** A command line into argv: split on whitespace, keeping quoted spans whole. */
export function splitCommand(line) {
  return [...String(line ?? '').matchAll(/"([^"]*)"|'([^']*)'|(\S+)/g)].map((m) => m[1] ?? m[2] ?? m[3]);
}

/** The command an `execute` tool call runs, for the exec policy: `rawInput.command` (a string, or
 * an argv array joined with spaces), else the title the agent showed. */
export function commandText(call) {
  const raw = call?.rawInput?.command;
  if (typeof raw === 'string' && raw.trim()) return raw;
  if (Array.isArray(raw) && raw.length) return raw.map(String).join(' ');
  return String(call?.title ?? '');
}

// The ACP tool kinds that only look at the world; §2's risk vocabulary only rounds up.
const READ_ONLY_KINDS = new Set(['read', 'search', 'fetch', 'think']);

/** The question's risk class from the ACP kind: read-only kinds are read_only, everything else
 * workspace_write (§2's higher classes need a story ACP cannot tell). */
export function riskForKind(kind) {
  return READ_ONLY_KINDS.has(String(kind ?? '')) ? 'read_only' : 'workspace_write';
}

/** The text of an ACP content block: `text` verbatim, any other block a named placeholder. */
export function contentText(block) {
  if (!block || typeof block !== 'object') return '';
  return block.type === 'text' ? String(block.text ?? '') : `[${block.type ?? 'unknown'}]`;
}

/** A `tool_call_update`'s payload as displayable text: raw output plus content blocks. */
export function toolOutput(update) {
  const parts = [];
  if (update.rawOutput !== undefined && update.rawOutput !== null) {
    parts.push(typeof update.rawOutput === 'string' ? update.rawOutput : JSON.stringify(update.rawOutput));
  }
  for (const block of Array.isArray(update.content) ? update.content : []) {
    if (block?.type === 'content') parts.push(contentText(block.content));
    else if (block?.type === 'diff') parts.push(`[diff ${String(block.path ?? '')}]`);
    else parts.push(JSON.stringify(block));
  }
  return parts.join('\n') || '{}';
}

/** A plan update as a thinking checklist — colonizer-runner/1 has no plan event (README). */
function planText(entries) {
  const mark = { completed: 'x', in_progress: ' ', pending: ' ' };
  return [
    'Plan:',
    ...(Array.isArray(entries) ? entries : []).map(
      (entry) => `- [${mark[String(entry?.status ?? 'pending')] ?? ' '}] ${String(entry?.content ?? '')}`,
    ),
  ].join('\n');
}

/** §2: every question carries 2-4 options. Below two, a synthetic Cancel pads the card; `synthetic`
 * marks the padding — answering it cancels, since it is no agent's option — and never hits the wire. */
export function clampOptions(options) {
  const list = (Array.isArray(options) ? options : [])
    .filter((option) => option && typeof option.optionId === 'string')
    .slice(0, 4)
    .map((option) => ({ optionId: option.optionId, name: String(option.name ?? option.optionId), kind: String(option.kind ?? 'other') }));
  while (list.length < 2) list.push({ optionId: '__cancel__', name: 'Cancel', kind: 'reject_once', synthetic: true });
  return list;
}

/** The option a policy decision answers with: the exact `allow_once`/`reject_once` kind, then any
 * of the prefix — the synthetic Cancel padding never answers on the policy's behalf. */
function optionByKind(options, prefix) {
  const real = options.filter((option) => !option.synthetic);
  return real.find((option) => option.kind === `${prefix}_once`) ?? real.find((option) => option.kind.startsWith(prefix));
}

/** The workspace-confined absolute path for `target`, or null when it escapes: symlinks resolve
 * through the longest existing ancestor, so a link out of the tree cannot hide an escape. */
export function confine(workspace, target) {
  let abs = resolve(workspace, String(target ?? ''));
  const tail = [];
  for (;;) {
    try {
      const resolved = join(realpathSync(abs), ...tail);
      return resolved === workspace || resolved.startsWith(workspace + sep) ? resolved : null;
    } catch {
      tail.unshift(basename(abs));
      const parent = dirname(abs);
      if (parent === abs) return null;
      abs = parent;
    }
  }
}

// §2 caps tool_result output; file reads refuse anything over READ_CAP instead of buffering it.
const TOOL_RESULT_LIMIT = 20000;
const READ_CAP = 16 * 1024 * 1024;
const clip = (text, limit = TOOL_RESULT_LIMIT) => (text.length > limit ? `${text.slice(0, limit - 1)}…` : text);
const plainObject = (value) => (value && typeof value === 'object' && !Array.isArray(value) ? value : {});
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
// JSON-RPC 2.0 error codes this runner replies with.
const NO_SUCH_METHOD = -32601;
const BAD_REQUEST = -32602;
// The agent→client methods this runner serves, by handler.
const TERMINAL_METHODS = new Set(['terminal/create', 'terminal/output', 'terminal/wait_for_exit', 'terminal/kill', 'terminal/release']);
const NO_COUNTERPART = new Set(['user_message_chunk', 'available_commands_update', 'current_mode_update']);

/** The ACP side of the wire over the agent's stdio: `request` resolves with the agent's result or
 * rejects with its error; agent→client requests land on `onRequest` (with `reply`/`replyError`),
 * `session/update` notifications on `onUpdate`; the first exit or spawn failure lands on `onDeath`
 * exactly once and rejects everything still pending. */
export function startAgent({ argv, env, workspace, emit, onUpdate, onRequest, onDeath, spawnFn = spawn }) {
  const child = spawnFn(argv[0], argv.slice(1), { cwd: workspace, env, stdio: ['pipe', 'pipe', 'pipe'] });
  let stderrTail = '';
  let failure = null;
  let nextId = 0;
  const pending = new Map(); // our request id -> { resolve, reject }
  const exited = new Promise((resolve) => {
    child.on('close', resolve);
    child.on('error', resolve); // a spawn failure never closes
  });
  child.stdin.on('error', () => {}); // EPIPE racing a death; the `failure` checks do the rest
  const write = (message) => {
    if (failure) return;
    child.stdin.write(`${JSON.stringify(message)}\n`);
  };
  const fail = (err) => {
    if (failure) return;
    failure = err;
    for (const waiter of pending.values()) waiter.reject(err);
    onDeath?.(err);
  };
  child.on('error', (err) => fail(Object.assign(new Error(`could not start ${JSON.stringify(argv[0])}: ${err?.message ?? err}`), { agentDied: true })));
  child.on('close', (code, signal) => {
    if (child.killed) return; // our own shutdown kill, not a death
    fail(Object.assign(new Error(`the ACP agent exited unexpectedly (exit code ${code ?? 'null'}${signal ? `, signal ${signal}` : ''})`), { agentDied: true }));
  });
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => (stderrTail = (stderrTail + chunk).slice(-2000)));

  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  lines.on('line', (line) => {
    if (!line.trim()) return;
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      return emit({ type: 'log', level: 'warn', message: `the agent emitted a line that is not JSON: ${clip(String(line), 200)}` });
    }
    if (typeof message.method !== 'string') {
      // A response to one of our requests (ids are independent per side, so anything carrying a
      // method is the agent's own request or notification, never a response to ours).
      const waiter = message.id !== undefined && pending.get(message.id);
      if (waiter) {
        pending.delete(message.id);
        if (message.error) waiter.reject(new Error(message.error.message ?? `agent error ${message.error.code ?? ''}`));
        else waiter.resolve(plainObject(message.result));
      }
      return;
    }
    if (message.method === 'session/update') return onUpdate(plainObject(message.params?.update), message.params);
    if (message.id === undefined) return; // a notification with no counterpart
    const reply = (result) => write({ jsonrpc: '2.0', id: message.id, result: result ?? {} });
    const replyError = (code, text) => write({ jsonrpc: '2.0', id: message.id, error: { code, message: text } });
    Promise.resolve(onRequest(message.method, plainObject(message.params), reply, replyError)).catch((err) =>
      replyError(-32603, `the runner failed to serve ${message.method}: ${err?.message ?? err}`),
    );
  });

  return {
    request(method, params) {
      if (failure) return Promise.reject(failure);
      const id = ++nextId;
      return new Promise((res, rej) => {
        pending.set(id, { resolve: res, reject: rej });
        write({ jsonrpc: '2.0', id, method, params });
      });
    },
    notify(method, params) {
      write({ jsonrpc: '2.0', method, params });
    },
    /** SIGTERM now, SIGKILL after `graceMs`; resolves when the agent is gone (its SIGTERM close is
     * not a death). The escalation timer is not unref'd on purpose: shutdown must not orphan an
     * agent that ignores SIGTERM, so callers await this before exiting. */
    kill(graceMs = 2000) {
      if (child.exitCode !== null || child.signalCode) return exited;
      child.kill('SIGTERM');
      const escalation = setTimeout(() => child.kill('SIGKILL'), graceMs);
      return exited.finally(() => clearTimeout(escalation));
    },
    failure: () => failure,
    stderrTail: () => stderrTail,
  };
}

/** A push-only async queue: `next()` resolves per command, `close()` ends the stream with null. */
class AsyncQueue {
  #pending = [];
  #waiting = [];
  #closed = false;

  push(value) {
    const waiter = this.#closed ? null : this.#waiting.shift();
    if (waiter) waiter.resolve(value);
    else if (!this.#closed) this.#pending.push(value);
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

/** The command loop. Returns the process exit code: 0 on shutdown or stdin EOF, 1 when the agent
 * died (a death mid-turn also fails the turn, so the colony's turn always terminates). */
export async function run({ commands, emit, env = process.env, spawnFn = spawn, cwd = process.cwd() }) {
  emit({ type: 'status', state: 'idle' });
  const { preset, argv } = agentArgv(env);
  let problem = null;
  if (!argv?.length) {
    problem =
      preset === 'custom'
        ? { code: AGENT_UNKNOWN, message: 'the custom command is empty; set the `command` setting to the ACP agent\'s full command line' }
        : { code: AGENT_UNKNOWN, message: `"${preset}" is not an ACP agent preset; pick one of ${Object.keys(PRESETS).join(', ')}, or "custom" with a command` };
  } else if (preset !== 'custom' && !String(env[PRESETS[preset].credential] ?? '').trim()) {
    problem = {
      code: CREDENTIAL_MISSING,
      message:
        `${PRESETS[preset].credential} is unset or empty, and the colony never runs the agent's interactive login. ` +
        `Add ${PRESETS[preset].credential} as a colony secret for the agent's API host (module.json's secrets list), ` +
        'so the mothership injects it into this colony; or pick another agent preset.',
    };
  }
  if (problem) {
    emit({ type: 'log', level: 'error', message: `${problem.code}: ${problem.message}` });
    emit({ type: 'status', state: 'error', detail: problem.code });
  }

  const workspace = realpathSync(cwd);
  // The layered exec policy (issue #471), loaded once at start: the repo layer's file is read
  // before the agent can run anything. Warnings ride stderr; agentd turns those into `log` events.
  const execPolicy = loadExecPolicy(env, { cwd: workspace });
  for (const warning of execPolicy.warnings) process.stderr.write(`${warning}\n`);
  // The mounted path policy (issue #647), loaded once: the bind list the guest booted with is what
  // the runtime reports against. Absent (an older harness) means the feature is off, silently.
  const pathPolicy = loadPathPolicy(env).policy;
  let sessionId = null, currentModel = null, modelSupported = false;
  let turn = null; // { messageId, text, thoughts } while a session/prompt is in flight
  let replaying = false; // a session/load in flight: its updates replay history the harness logged
  let dead = false;
  let turnCount = 0, permissionCount = 0, terminalCount = 0, activePump = null;
  const queued = [];
  const open = new Map(); // question_id -> { resolve } while a question awaits its answer
  const terminals = new Map(); // terminalId -> { child, chunks, size, truncated, exit, waiters }

  const markDead = () => {
    if (dead) return;
    dead = true;
    const tail = acp.stderrTail().trim();
    emit({ type: 'log', level: 'error', message: `${AGENT_FAILED}: ${acp.failure()?.message ?? 'the ACP agent died'}${tail ? `; stderr tail: ${clip(tail, 500)}` : ''}` });
    emit({ type: 'status', state: 'error', detail: AGENT_FAILED });
    commands.close();
  };

  /** `session/update` → protocol events (README, "Mapping"). */
  const onUpdate = (update) => {
    if (!turn || replaying) return; // an update outside a turn (or replayed history) has nothing to attach to
    switch (String(update.sessionUpdate ?? '')) {
      case 'agent_message_chunk': {
        const text = contentText(update.content);
        if (!text) break;
        turn.text += text;
        emit({ type: 'assistant_text_delta', message_id: turn.messageId, block_index: 0, delta: text });
        break;
      }
      case 'agent_thought_chunk':
        turn.thoughts.push(contentText(update.content)); // one thinking event at turn end
        break;
      case 'tool_call':
        emit({ type: 'tool_call', message_id: turn.messageId, tool_call_id: String(update.toolCallId ?? ''), name: String(update.title ?? update.kind ?? 'tool'), input: plainObject(update.rawInput) });
        break;
      case 'tool_call_update':
        // pending/in_progress are progress only; the result follows in a later update
        if (update.status === 'completed' || update.status === 'failed') {
          emit({ type: 'tool_result', tool_call_id: String(update.toolCallId ?? ''), output: clip(toolOutput(update)), is_error: update.status === 'failed' });
        }
        break;
      case 'plan':
        emit({ type: 'thinking', message_id: turn.messageId, block_index: 1, text: planText(update.entries) });
        break;
      default:
        // known updates with no protocol counterpart are ignored; anything else is named
        if (!NO_COUNTERPART.has(String(update.sessionUpdate ?? ''))) emit({ type: 'log', level: 'warn', message: `ignored an unknown session/update type: ${JSON.stringify(update.sessionUpdate ?? null)}` });
    }
  };

  /** `session/request_permission` → a question card; the `answer` command (or an interrupt) picks
   * the outcome. Free text cannot select an ACP option, and neither can the padded Cancel, so both
   * answer cancelled. Before the card, an `execute` call meets the exec policy: a deny answers the
   * reject option, an allow the allow one, and only an ask (or a call with neither option) reaches
   * the card, with the rule named on it. */
  const onPermission = async (params, reply) => {
    const call = plainObject(params.toolCall);
    const options = clampOptions(params.options);
    // Only asks reach the card: commands the agent runs without asking are never fenced, so the
    // policy is guidance here, like in Claude Code — the colony VM is the boundary.
    const command = call.kind === 'execute' ? commandText(call) : null;
    const hit = command ? evaluateExecPolicy(execPolicy, command, { cwd: workspace }) : null;
    if (hit) {
      process.stderr.write(`${execPolicyLogLine(hit, command)}\n`);
      if (hit.decision === 'deny') {
        const reject = optionByKind(options, 'reject');
        return reply(reject ? { outcome: { outcome: 'selected', optionId: reject.optionId } } : { outcome: { outcome: 'cancelled' } });
      }
      const allow = optionByKind(options, 'allow');
      if (hit.decision === 'allow' && allow) return reply({ outcome: { outcome: 'selected', optionId: allow.optionId } });
    }
    const title = String(call.title ?? '').trim() || `Allow ${call.kind ?? 'this tool call'}?`;
    const text = hit ? `${title} — ${execPolicyReason(hit)}` : title;
    const questionId = String(call.toolCallId ?? '') || `permission-${++permissionCount}`;
    emit({
      type: 'question', question_id: questionId, message_id: turn?.messageId ?? null, risk: riskForKind(call.kind),
      questions: [{ question: text, header: 'Permission', multi_select: false, options: options.map((o) => ({ label: o.name, description: o.kind })) }],
    });
    emit({ type: 'status', state: 'waiting_for_answer' });
    const answer = await new Promise((resolve) => open.set(questionId, { resolve })); // null on interrupt
    open.delete(questionId);
    if (!answer) return reply({ outcome: { outcome: 'cancelled' } });
    emit({ type: 'question_answered', question_id: questionId, answers: answer.answers, response: answer.response });
    emit({ type: 'status', state: 'working' });
    const chosen = options.find((option) => option.name === answer.answers[text]);
    if (!chosen && answer.response) {
      emit({ type: 'log', level: 'info', message: `a free-text reply cannot select one of the agent's options; answered cancelled for ${questionId}` });
    }
    reply({ outcome: chosen && !chosen.synthetic ? { outcome: 'selected', optionId: chosen.optionId } : { outcome: 'cancelled' } });
  };

  const refuse = (replyError, path, why) => replyError(BAD_REQUEST, `refused: ${JSON.stringify(path ?? '')} ${why}`);
  // The path policy's runtime report (issue #647): a file request that lands on a masked or
  // protected path emits one `path_policy` event per (access, path) per run. Reporting only — the
  // reply is never touched; the mount enforced before this ran. `path` is already confined to the
  // workspace by `confine`, so the relative form is the worktree-relative path the policy names.
  const pathPolicySeen = new Set();
  const reportPathPolicy = (access, tool, path) => {
    if (!pathPolicy) return;
    const rel = relative(workspace, path).split(sep).join('/');
    const rule = matchPathPolicy(pathPolicy, rel);
    if (!rule || (access === 'read' && rule === 'protected')) return;
    const key = `${access}\u0000${rel}`;
    if (pathPolicySeen.has(key)) return;
    pathPolicySeen.add(key);
    emit({ type: 'path_policy', access, policy: rule, path: rel, tool });
  };
  const readTextFile = async (params, reply, replyError) => {
    const path = confine(workspace, params.path);
    if (!path) return refuse(replyError, params.path, 'is outside the workspace');
    reportPathPolicy('read', 'fs/read_text_file', path);
    const info = await stat(path).catch(() => null);
    if (info && info.size > READ_CAP) return refuse(replyError, params.path, `is ${info.size} bytes, over the ${READ_CAP / (1024 * 1024)} MiB read cap`);
    let content;
    try {
      content = await readFile(path, 'utf8');
    } catch (err) {
      return replyError(BAD_REQUEST, `could not read ${JSON.stringify(params.path)}: ${err?.message ?? err}`);
    }
    const lines = content.split('\n');
    const from = Number.isInteger(params.line) && params.line > 0 ? params.line : 1;
    const limit = Number.isInteger(params.limit) && params.limit >= 0 ? params.limit : lines.length;
    reply({ content: lines.slice(from - 1, from - 1 + limit).join('\n') });
  };

  const writeTextFile = async (params, reply, replyError) => {
    const path = confine(workspace, params.path);
    if (!path) return refuse(replyError, params.path, 'is outside the workspace');
    reportPathPolicy('write', 'fs/write_text_file', path);
    try {
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, String(params.content ?? ''), 'utf8');
    } catch (err) {
      return replyError(BAD_REQUEST, `could not write ${JSON.stringify(params.path)}: ${err?.message ?? err}`);
    }
    reply({});
  };

  /** The terminal/* surface: each command is started with a cwd inside the workspace (the VM is the
   * boundary), output capped at `outputByteLimit` (default 16 000 bytes), truncated from the front. */
  const onTerminal = async (method, params, reply, replyError) => {
    const terminal = terminals.get(String(params.terminalId ?? ''));
    if (method !== 'terminal/create' && !terminal) {
      return replyError(BAD_REQUEST, `no terminal with id ${JSON.stringify(params.terminalId ?? '')}`);
    }
    if (method === 'terminal/create') {
      const cwd = params.cwd === undefined || params.cwd === null ? workspace : confine(workspace, params.cwd);
      if (!cwd) return refuse(replyError, params.cwd, 'is outside the workspace');
      const limit = Number(params.outputByteLimit) > 0 ? Number(params.outputByteLimit) : 16000;
      const entry = { chunks: [], size: 0, truncated: false, exit: null, waiters: [], child: null };
      const push = (chunk) => {
        entry.size += chunk.length;
        entry.chunks.push(chunk);
        entry.truncated ||= entry.size > limit;
        while (entry.size > limit && entry.chunks.length) {
          const drop = entry.size - limit;
          if (entry.chunks[0].length > drop) {
            entry.chunks[0] = entry.chunks[0].subarray(drop);
            entry.size = limit;
          } else {
            entry.size -= entry.chunks[0].length;
            entry.chunks.shift();
          }
        }
      };
      const finish = (exit) => {
        if (entry.exit) return;
        entry.exit = exit;
        for (const waiter of entry.waiters.splice(0)) waiter(exit);
      };
      entry.child = spawnFn(String(params.command ?? ''), Array.isArray(params.args) ? params.args.map(String) : [], {
        cwd,
        env: { ...env, ...Object.fromEntries((Array.isArray(params.env) ? params.env : []).filter((v) => v && typeof v.name === 'string').map((v) => [v.name, String(v.value ?? '')])) },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      entry.child.stdout.on('data', push);
      entry.child.stderr.on('data', push);
      entry.child.on('error', (err) => {
        push(Buffer.from(`could not run ${JSON.stringify(params.command)}: ${err?.message ?? err}`));
        finish({ exitCode: null, signal: null });
      });
      entry.child.on('close', (code, signal) => finish({ exitCode: code, signal }));
      terminals.set(`term-${++terminalCount}`, entry);
      return reply({ terminalId: `term-${terminalCount}` });
    }
    if (method === 'terminal/output') reply({ output: Buffer.concat(terminal.chunks).toString('utf8'), truncated: terminal.truncated, ...(terminal.exit ? { exitStatus: terminal.exit } : {}) });
    else if (method === 'terminal/wait_for_exit') reply(terminal.exit ?? (await new Promise((resolve) => terminal.waiters.push(resolve))));
    else if (method === 'terminal/kill' || method === 'terminal/release') {
      if (!terminal.exit) terminal.child.kill('SIGTERM');
      if (method === 'terminal/release') terminals.delete(String(params.terminalId ?? ''));
      reply({});
    }
  };

  const onRequest = (method, params, reply, replyError) => {
    if (method === 'session/request_permission') return onPermission(params, reply);
    if (method === 'fs/read_text_file') return readTextFile(params, reply, replyError);
    if (method === 'fs/write_text_file') return writeTextFile(params, reply, replyError);
    if (TERMINAL_METHODS.has(method)) return onTerminal(method, params, reply, replyError);
    return replyError(NO_SUCH_METHOD, `method not found: ${method}`);
  };

  let acp = null;
  if (!problem) acp = startAgent({ argv, env, workspace, emit, spawnFn, onUpdate, onRequest, onDeath: markDead });

  // The handshake: negotiate ACP, then the session — `session/load` for §1's COLONIZER_RESUME_SESSION
  // when the agent advertises loadSession, `session/new` otherwise and as the fallback on a failed
  // load. agent_session is announced only when the session could be resumed again (§2 rules).
  if (acp) {
    try {
      const init = await acp.request('initialize', { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: true, writeTextFile: true }, terminal: true } });
      if (init.protocolVersion !== 1) {
        emit({ type: 'log', level: 'warn', message: `the agent speaks ACP protocol version ${JSON.stringify(init.protocolVersion)}, this runner negotiates 1` });
      }
      const loadable = Boolean(plainObject(init.agentCapabilities).loadSession);
      const resumeId = String(env.COLONIZER_RESUME_SESSION ?? '').trim();
      if (resumeId && !loadable) {
        emit({ type: 'log', level: 'warn', message: `cannot resume session ${resumeId}: the agent does not advertise loadSession; a fresh session starts instead` });
      }
      let session = {};
      if (loadable && resumeId) {
        replaying = true; // the agent replays the old conversation; the harness logged it once already
        try {
          // The load result carries what session/new would (models included), so a resumed colony
          // keeps its model surface.
          session = plainObject(await acp.request('session/load', { sessionId: resumeId, cwd: workspace, mcpServers: [] }));
          // The agent may rename the session as it loads it; talk to the id it answered with.
          sessionId = typeof session.sessionId === 'string' && session.sessionId ? session.sessionId : resumeId;
        } catch (err) {
          emit({ type: 'log', level: 'warn', message: `could not resume session ${resumeId} (${err?.message ?? err}); a fresh session starts instead` });
        }
        replaying = false;
      }
      if (!sessionId) {
        session = await acp.request('session/new', { cwd: workspace, mcpServers: [] });
        sessionId = String(session.sessionId ?? '');
      }
      if (loadable && sessionId) emit({ type: 'agent_session', session_id: sessionId });
      modelSupported = Boolean(session.models);
      if (session.models?.currentModelId) {
        currentModel = String(session.models.currentModelId);
        emit({ type: 'model_changed', model: currentModel, previous: null });
      }
    } catch (err) {
      if (!dead) {
        emit({ type: 'log', level: 'error', message: `${AGENT_FAILED}: the ACP handshake failed: ${err?.message ?? err}` });
        emit({ type: 'status', state: 'error', detail: AGENT_FAILED });
      }
      return 1;
    }
  }

  const pump = () => {
    if (activePump) return activePump;
    activePump = (async () => {
      try {
        while (queued.length) {
          const message = queued.shift();
          emit({ type: 'status', state: 'working' });
          const startedAt = Date.now();
          if (problem) {
            emit({ type: 'turn_end', is_error: true, result: `${problem.code}: ${problem.message}`, cost_usd: null, duration_ms: 0 });
            emit({ type: 'status', state: 'error', detail: problem.code });
            continue;
          }
          turnCount += 1;
          turn = { messageId: `msg-${turnCount}`, text: '', thoughts: [] };
          let failureText = null;
          let stopped = null;
          try {
            const result = await acp.request('session/prompt', { sessionId, prompt: [{ type: 'text', text: message.text }] });
            const reason = String(result.stopReason ?? '');
            stopped = reason === 'cancelled' ? 'interrupted by the user' : reason === 'refusal' ? 'the agent refused to continue' : null;
            if (!stopped && reason !== 'end_turn') emit({ type: 'log', level: 'warn', message: `the turn stopped on ${JSON.stringify(result.stopReason)}` });
          } catch (err) {
            if (err?.agentDied) {
              // markDead already logged the death; this turn must still end, as an error.
              emit({ type: 'turn_end', is_error: true, result: `the ACP agent died mid-turn: ${err?.message ?? err}`, cost_usd: null, duration_ms: Date.now() - startedAt });
              return;
            }
            failureText = `the turn failed: ${err?.message ?? err}`;
          }
          if (!failureText) {
            if (turn.thoughts.length) emit({ type: 'thinking', message_id: turn.messageId, block_index: 1, text: turn.thoughts.join('\n') });
            if (turn.text) emit({ type: 'assistant_text', message_id: turn.messageId, block_index: 0, text: turn.text });
          }
          const result = failureText ?? stopped ?? (turn.text || null);
          turn = null;
          emit({ type: 'turn_end', is_error: Boolean(failureText ?? stopped), result, cost_usd: null, duration_ms: Date.now() - startedAt });
          emit({ type: 'status', state: 'idle' });
        }
      } finally {
        activePump = null;
      }
    })();
    return activePump;
  };

  for (;;) {
    const command = await commands.next();
    if (dead) return 1; // a death while we awaited (or mid-command): markDead already reported it
    if (!command || command.type === 'shutdown') break;
    switch (command.type) {
      case 'user_message': {
        if (typeof command.text !== 'string' || !command.text.trim()) {
          emit({ type: 'log', level: 'warn', message: 'ignored a user_message without text' });
          break;
        }
        emit({ type: 'user_message', id: command.id, text: command.text });
        queued.push(command);
        pump();
        break;
      }
      case 'interrupt':
        acp?.notify('session/cancel', { sessionId });
        for (const [id, entry] of [...open]) {
          open.delete(id);
          entry.resolve(null);
        }
        break;
      case 'set_model': {
        const model = typeof command.model === 'string' ? command.model.trim() : '';
        if (!model) {
          emit({ type: 'log', level: 'warn', message: 'ignored a set_model without a model' });
        } else if (!acp || !modelSupported) {
          emit({ type: 'log', level: 'warn', message: `ignored set_model ${model}: the agent did not advertise model selection at session/new` });
        } else {
          try {
            await acp.request('session/set_model', { sessionId, modelId: model });
            emit({ type: 'model_changed', model, previous: currentModel });
            currentModel = model;
          } catch (err) {
            emit({ type: 'log', level: 'warn', message: `set_model ${model} failed: ${err?.message ?? err}` });
          }
        }
        break;
      }
      case 'answer': {
        const entry = open.get(String(command.question_id ?? ''));
        if (!entry) {
          emit({ type: 'log', level: 'warn', message: `no open question with id ${command.question_id}` });
          break;
        }
        entry.resolve({
          answers: plainObject(command.answers),
          response: typeof command.response === 'string' && command.response.trim() ? command.response : null,
        });
        break;
      }
      default:
        break; // unknown commands are ignored (protocol forward compatibility)
    }
  }

  // Shutdown or stdin EOF: land a running turn (the agent answers session/cancel with the
  // `cancelled` stop reason), then stop the agent and wait for it to be gone.
  if (acp) {
    if (activePump) {
      acp.notify('session/cancel', { sessionId });
      await Promise.race([activePump, sleep(2000)]);
    }
    await acp.kill(); // bounded by the SIGKILL escalation inside
  }
  emit({ type: 'status', state: 'exited' });
  return 0;
}

async function main() {
  const emit = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
  const commands = new AsyncQueue();

  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  lines.on('line', (line) => {
    if (!line.trim()) return;
    try { commands.push(JSON.parse(line)); } catch { emit({ type: 'log', level: 'warn', message: 'ignored a command line that is not valid JSON' }); }
  });
  lines.on('close', () => commands.close());
  for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => commands.push({ type: 'shutdown' }));

  process.exit(await run({ commands, emit, env: process.env }));
}

let isEntrypoint = false;
try {
  isEntrypoint = realpathSync(process.argv[1]) === fileURLToPath(import.meta.url);
} catch {
  // no argv[1]: not the entrypoint
}

if (isEntrypoint) {
  main().catch((err) => {
    process.stderr.write(`${err?.stack ?? err}\n`);
    process.stdout.write(`${JSON.stringify({ type: 'status', state: 'exited', detail: String(err?.message ?? err) })}\n`);
    process.exit(1);
  });
}
