#!/usr/bin/env node
// Colonizer agent runner for OpenCode. Contract (docs/protocol.md §2): commands on stdin
// as JSONL, events out on stdout, diagnostics on stderr. Each user_message starts a turn
// (`opencode run --format json --model <provider>/<model>`, --session from turn two on).
// Models come from Settings → Providers via the gateway (§6.5); questions go through our own
// MCP tool, because OpenCode's native question tool is unavailable in `run` mode.

import { execFile, spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

export const OPENCODE_VERSION = '1.18.32';
export const MAX_TOOL_OUTPUT = 20_000;
export const FIRST_OUTPUT_MS = 120_000; // a stalled first run is SIGINTed and retried once
const SHUTDOWN_GRACE_MS = 8000;
const MCP_TIMEOUT_MS = 3_600_000; // MCP requests default to 5 s; asks wait on a human for minutes
const DEFAULT_CONTEXT_TOKENS = 128_000;
const AUTH_MODES = new Set(['x-api-key', 'bearer', 'none']);
const NO_ROUTE_HINT = 'OpenCode colonies reach models through Settings → Providers; set the agent model to <provider>/<model>, e.g. local/deepseek-v4-flash';

/** Minimal async queue usable as an AsyncIterable (stdin commands). */
export class AsyncQueue {
  #items = []; #waiters = []; #closed = false;
  push(v) { if (this.#closed) return false; const w = this.#waiters.shift(); if (w) w({ value: v, done: false }); else this.#items.push(v); return true; }
  close() { this.#closed = true; for (const w of this.#waiters.splice(0)) w({ value: undefined, done: true }); }
  [Symbol.asyncIterator]() { return { next: () => (this.#items.length ? Promise.resolve({ value: this.#items.shift(), done: false }) : this.#closed ? Promise.resolve({ value: undefined, done: true }) : new Promise((r) => this.#waiters.push(r))) }; }
}

/** COLONIZER_MODEL_ROUTES → validated routes, with warnings instead of throwing. */
export function parseRoutes(raw) {
  let data;
  try { data = JSON.parse(raw); } catch { return { routes: [], warnings: ['ignoring COLONIZER_MODEL_ROUTES: not valid JSON'] }; }
  if (!Array.isArray(data)) return { routes: [], warnings: ['ignoring COLONIZER_MODEL_ROUTES: expected a JSON array'] };
  const routes = []; const warnings = [];
  data.forEach((entry, i) => {
    const prefix = typeof entry?.prefix === 'string' ? entry.prefix : '';
    const baseUrl = typeof entry?.base_url === 'string' ? entry.base_url : '';
    if (!prefix.endsWith('/') || prefix.length < 2 || !/^https?:\/\//.test(baseUrl) || !AUTH_MODES.has(entry?.auth ?? 'x-api-key')) {
      warnings.push(`ignoring model route ${i}: needs a "<provider>/" prefix, an http(s) base_url and auth x-api-key|bearer|none`);
      return;
    }
    const headers = {};
    if (entry?.headers && typeof entry.headers === 'object') for (const [name, value] of Object.entries(entry.headers)) if (/^[A-Za-z0-9-]+$/.test(name) && typeof value === 'string') headers[name.toLowerCase()] = value;
    const num = (v) => (Number.isInteger(v) && v > 0 ? v : null);
    routes.push({ provider: typeof entry.provider === 'string' && entry.provider ? entry.provider : prefix.slice(0, -1), prefix, base_url: baseUrl, headers, timeout_secs: num(entry.timeout_secs), context_tokens: num(entry.context_tokens) });
  });
  return { routes, warnings };
}

/** Split `<provider>/<model>` on the first `/` and match the prefix to a route. */
export function splitModel(model, routes) {
  const at = String(model ?? '').indexOf('/');
  if (at < 1) return { error: 'bare' };
  const route = routes.find((r) => String(model).startsWith(r.prefix));
  if (!route) return { error: 'unrouted', provider: String(model).slice(0, at) };
  return { route, name: String(model).slice(at + 1) };
}

/** Fatal preflight: null when the model can run, otherwise the error for the log. */
export function preflight(model, routes) {
  const trimmed = String(model ?? '').trim();
  if (!trimmed) return `${NO_ROUTE_HINT} (no model is set)`;
  const split = splitModel(trimmed, routes);
  if (split.error === 'bare') return `${NO_ROUTE_HINT} ("${trimmed}" names no provider)`;
  if (split.error === 'unrouted') return `${NO_ROUTE_HINT} (provider "${split.provider}" has no route)`;
  if (!split.name) return `${NO_ROUTE_HINT} ("${trimmed}" names no model after the /)`;
  return null;
}

/** Inline config (OPENCODE_CONFIG_CONTENT): one `@ai-sdk/anthropic` provider per route in use,
 * pointing at the gateway, plus the colonizer MCP server. `timeout` maps `timeout_secs` to ms. */
export function opencodeConfig({ routes, model, smallModel, mcp }) {
  const small = smallModel || model;
  const used = [];
  for (const m of [model, small]) { const s = splitModel(m, routes); if (!s.error && !used.some((u) => u.route.provider === s.route.provider)) used.push(s); }
  const provider = {};
  for (const { route } of used) provider[route.provider] = { npm: '@ai-sdk/anthropic', name: route.provider, options: { baseURL: `${route.base_url.replace(/\/+$/, '')}/v1`, apiKey: 'colonizer', headers: route.headers, ...(route.timeout_secs ? { timeout: route.timeout_secs * 1000 } : {}) }, models: {} };
  for (const m of [model, small]) {
    const s = splitModel(m, routes);
    if (s.error) continue;
    const context = s.route.context_tokens ?? DEFAULT_CONTEXT_TOKENS;
    provider[s.route.provider].models[s.name] = { name: s.name, limit: { context, output: Math.min(32_000, Math.floor(context / 4)) } };
  }
  return { provider, model, small_model: small, permission: 'allow', autoupdate: false, share: 'disabled', ...(mcp ? { mcp } : {}) };
}

// What the model needs that the config cannot say. Memory files are read, never written.
export const INSTRUCTIONS = [
  'You run inside the Colonizer; the user follows along in a web UI.',
  '- Whenever you need a decision, a clarification or any other input from the user, call the colonizer_ask_user tool with 2-4 concrete options, each a short label plus a one-sentence description. Never ask the user in plain text, and never end a turn with a plain-text question.',
  '- Do not run `git commit` or `git push` and do not create branches; the harness commits your changes and opens the pull request.',
  '- Shared memory notes are readable under $COLONIZER_MEMORY_DIR ({repo,org,global}/notes/*.md). Propose one with colonizer_memory_propose; send problems outside your task to colonizer_finding_file with how you confirmed them.',
].join('\n');

/** The lock row for this machine: arm64 takes linux-arm64, x64 the AVX2 build or baseline. */
export function archPlatform({ arch = process.arch, cpuinfo = '' } = {}) {
  if (arch === 'arm64') return 'linux-arm64';
  if (arch === 'x64') return /\bavx2\b/i.test(cpuinfo) ? 'linux-x64' : 'linux-x64-baseline';
  return null;
}

const execTar = (args) => new Promise((resolve, reject) => { execFile('tar', args, (error) => (error ? reject(error) : resolve())); });

/** Disk cache for the binary: the colony's /tmp is a small tmpfs, so the ~60 MB tarball and
 * ~185 MB binary live under the cache dir instead (os.tmpdir() only when HOME is unset). */
export function defaultCacheDir(env = process.env) {
  const base = env.XDG_CACHE_HOME || (env.HOME ? join(env.HOME, '.cache') : null);
  return base ? join(base, 'colonizer', 'opencode') : join(tmpdir(), 'colonizer-opencode');
}

/** The opencode binary: COLONIZER_OPENCODE_BIN, then PATH, then the pinned build for this
 * arch — downloaded from registry.npmjs.org, sha256-checked before extraction. */
export async function resolveOpencode({ env = process.env, lockText, arch = process.arch, cpuinfo = null, fetchImpl = fetch, runTar = execTar, cacheDir = defaultCacheDir(env), log = () => {} } = {}) {
  if (env.COLONIZER_OPENCODE_BIN) return env.COLONIZER_OPENCODE_BIN;
  for (const dir of String(env.PATH ?? '').split(':')) if (dir && existsSync(join(dir, 'opencode'))) return join(dir, 'opencode');
  if (cpuinfo === null) { try { cpuinfo = readFileSync('/proc/cpuinfo', 'utf8'); } catch { cpuinfo = ''; } }
  const platform = archPlatform({ arch, cpuinfo });
  const row = String(lockText ?? '').split('\n').map((l) => l.trim().split(/\s+/)).filter((c) => c.length >= 6 && !c[0].startsWith('#')).map(([, version, p, , sha256, url]) => ({ version, platform: p, sha256, url })).find((r) => r.platform === platform && r.version === OPENCODE_VERSION);
  if (!row) throw new Error(`no pinned OpenCode ${OPENCODE_VERSION} build for platform ${platform ?? arch}`);
  const dest = join(cacheDir, row.version, row.platform);
  const bin = join(dest, 'opencode');
  if (existsSync(bin)) return bin;
  log({ level: 'info', message: `downloading OpenCode ${row.version} (${row.platform})` });
  const res = await fetchImpl(row.url);
  if (!res?.ok) throw new Error(`OpenCode download failed: HTTP ${res?.status ?? 'no response'}`);
  const bytes = Buffer.from(await res.arrayBuffer());
  if (createHash('sha256').update(bytes).digest('hex') !== row.sha256) throw new Error(`OpenCode ${row.version} (${row.platform}) refused: sha256 mismatch`);
  mkdirSync(dest, { recursive: true });
  const tgz = join(dest, 'pkg.tgz');
  writeFileSync(tgz, bytes);
  try {
    await runTar(['-xzf', tgz, '-C', dest, 'package/bin/opencode']);
    renameSync(join(dest, 'package', 'bin', 'opencode'), bin);
  } finally {
    rmSync(join(dest, 'package'), { recursive: true, force: true });
    rmSync(tgz, { force: true }); // gone on success and on failure: it is 60 MB of tmpfs otherwise
  }
  chmodSync(bin, 0o755);
  return bin;
}

/** One `opencode run --format json` line → protocol events (mutating turn state `st`).
 * colonizer_* tool completions are skipped: already question/finding/memory events. */
export function mapLine(obj, st) {
  const cap = (t) => { const s = typeof t === 'string' ? t : t == null ? '' : JSON.stringify(t); return s.length <= MAX_TOOL_OUTPUT ? s : `${s.slice(0, MAX_TOOL_OUTPUT - 60)}\n… [truncated ${s.length - MAX_TOOL_OUTPUT} characters]`; };
  const out = [];
  if (obj?.sessionID && !st.sessionId) st.sessionId = obj.sessionID;
  const part = obj?.part ?? {};
  const mid = part.messageID ?? `m-${st.msg}`;
  if (obj?.type === 'text' && typeof part.text === 'string' && part.text) {
    st.lastText = part.text;
    out.push({ type: 'assistant_text', message_id: mid, block_index: st.block++, text: part.text });
  } else if (obj?.type === 'reasoning' && typeof part.text === 'string' && part.text) {
    out.push({ type: 'thinking', message_id: mid, block_index: st.block++, text: part.text });
  } else if (obj?.type === 'tool_use' && part?.state) {
    const name = String(part.tool ?? '');
    if (!name.startsWith('colonizer_')) {
      const id = String(part.callID ?? `call-${st.block}`);
      const isError = part.state.status === 'error';
      out.push({ type: 'tool_call', message_id: String(mid), tool_call_id: id, name, input: part.state.input ?? {} });
      out.push({ type: 'tool_result', tool_call_id: id, output: cap(isError ? (part.state.error ?? '') : (part.state.output ?? part.state.title ?? '')), is_error: isError });
    }
  } else if (obj?.type === 'step_finish' && part?.tokens) {
    const u = (st.usage[st.model] ??= { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 });
    u.input_tokens += part.tokens.input ?? 0; u.output_tokens += part.tokens.output ?? 0;
    u.cache_read_tokens += part.tokens.cache?.read ?? 0; u.cache_write_tokens += part.tokens.cache?.write ?? 0;
  } else if (obj?.type === 'error' && obj?.error) {
    st.failed = String(obj.error?.data?.message ?? obj.error?.message ?? obj.error?.name ?? 'unknown error');
    out.push({ type: 'log', level: 'error', message: st.failed });
  }
  return out;
}

/** Loopback HTTP bridge to mcp.mjs: asks wait for the matching `answer` command. */
export async function createBridge({ emit, setStatus, isWorking, findings = false, token = randomBytes(16).toString('hex') }) {
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
      if (req.url === '/ask') {
        const questionId = `q-${++count}`;
        pending.set(questionId, (answers) => reply(200, answers ?? { cancelled: true }));
        const qs = Array.isArray(msg.questions) ? msg.questions : [];
        emit({ type: 'question', question_id: questionId, message_id: typeof msg.message_id === 'string' ? msg.message_id : null, questions: qs.map((q) => ({ question: String(q?.question ?? ''), header: String(q?.header ?? q?.question ?? ''), multi_select: Boolean(q?.multiSelect), options: (Array.isArray(q?.options) ? q.options : []).map((o) => ({ label: String(o?.label ?? ''), description: String(o?.description ?? ''), preview: null })) })) });
        setStatus('waiting_for_answer');
      } else if (req.url === '/finding') {
        const missing = ['title', 'body', 'evidence'].filter((k) => typeof msg[k] !== 'string' || !msg[k].trim());
        if (missing.length) reply(200, { error: `finding_file needs ${missing.join(', ')}` });
        else {
          if (findings) emit({ type: 'finding', title: msg.title, body: msg.body, evidence: msg.evidence });
          reply(200, { filed: findings });
        }
      } else if (req.url === '/memory') {
        const scope = msg.scope ?? 'repo';
        if (!['repo', 'org', 'global'].includes(scope)) reply(200, { error: 'memory_propose scope must be repo, org or global' });
        else if (typeof msg.title !== 'string' || !msg.title.trim() || typeof msg.content !== 'string' || !msg.content.trim()) reply(200, { error: 'memory_propose needs a title and content' });
        else {
          emit({ type: 'memory_proposal', scope, title: msg.title, content: msg.content, tags: Array.isArray(msg.tags) ? msg.tags.map(String) : [] });
          reply(200, { ok: true });
        }
      } else reply(404, {});
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

/** Turns until `shutdown` or stdin EOF: each turn spawns `opencode run` with stdin prompt. */
export async function runAgent({ commands, emit, spawnImpl = spawn, bin, model, routes, makeEnv = (b) => ({ ...process.env, COLONIZER_BRIDGE_URL: b.url, COLONIZER_BRIDGE_TOKEN: b.token }), findings = false, bridgeToken, graceMs = SHUTDOWN_GRACE_MS, firstOutputMs = FIRST_OUTPUT_MS, onReady } = {}) {  let status = null, turnActive = false, closing = false, sessionId = null, currentModel = model, current = null, curState = null, active = false, turnPromise = null, messageCount = 0;
  const usage = {}; const queue = [];
  const secrets = [...new Set(routes.flatMap((r) => Object.values(r.headers ?? {})))].filter((v) => v);
  const redact = (s) => { let t = String(s); for (const v of secrets) t = t.split(v).join('[redacted]'); return t; };
  const setStatus = (state, detail) => { if (state === status && detail === undefined) return; status = state; emit(detail === undefined ? { type: 'status', state } : { type: 'status', state, detail }); };
  const bridge = await createBridge({ emit, setStatus, isWorking: () => turnActive, findings, token: bridgeToken });
  onReady?.(bridge);

  const runTurn = async (text) => {
    setStatus('working');
    const started = Date.now();
    const st = { model: currentModel, usage, sessionId, lastText: null, failed: null, cancelled: false, block: 0, msg: ++messageCount };
    const env = makeEnv(bridge, st.model); // rebuilt per turn so a set_model model is configured
    let code = 0;
    for (let attempt = 0; attempt < 2 && !closing; attempt++) {
      const title = String(text).split('\n').map((l) => l.trim()).find(Boolean)?.slice(0, 80) ?? 'Colonizer turn';
      const args = ['run', '--format', 'json', '--model', st.model, ...(sessionId ? ['--session', sessionId] : []), '--title', title];
      const r = await new Promise((resolve) => {
        const child = spawnImpl(bin, args, { cwd: process.cwd(), env, stdio: ['pipe', 'pipe', 'pipe'] });
        current = child; curState = st;
        let output = false, done = false, tail = '';
        const onLine = (line) => { if (!line.trim()) return; let obj; try { obj = JSON.parse(line); } catch { return; } output = true; for (const event of mapLine(obj, st)) emit(event); };
        const finish = (c) => { if (done) return; done = true; clearTimeout(timer); current = null; curState = null; bridge.cancelAll(); resolve({ output, code: c }); };
        const timer = setTimeout(() => { if (!output) child.kill('SIGINT'); }, firstOutputMs);
        timer.unref?.();
        child.stdout.on('data', (chunk) => {
          const lines = (tail + String(chunk)).split('\n');
          tail = lines.pop();
          for (const line of lines) onLine(line);
        });
        child.stderr?.on('data', (data) => process.stderr.write(redact(data)));
        child.on('error', () => finish(1));
        // A signal death reports a null code: map it to the conventional number (SIGINT → 130).
        child.on('exit', (c, signal) => { onLine(tail); tail = ''; finish(c ?? { SIGINT: 130, SIGTERM: 143, SIGKILL: 137 }[signal] ?? 1); });
        try { child.stdin.write(text); child.stdin.end(); } catch { finish(1); }
      });
      code = r.code;
      if (r.output || attempt === 1 || st.cancelled || closing) break;
      emit({ type: 'log', level: 'warn', message: `OpenCode produced no output within ${firstOutputMs / 1000} s; retrying the turn once` });
    }
    if (st.sessionId) sessionId = st.sessionId;
    const modelUsage = {};
    for (const [m, u] of Object.entries(st.usage)) modelUsage[m] = { ...u };
    emit({ type: 'turn_end', is_error: Boolean(st.failed) || (code !== 0 && !(st.cancelled && code === 130)), result: st.failed ?? st.lastText ?? null, cost_usd: 0, duration_ms: Date.now() - started, ...(Object.keys(modelUsage).length ? { model_usage: modelUsage } : {}) });
  };

  const pump = () => {
    if (active || !queue.length || closing) return;
    active = true; turnActive = true;
    turnPromise = runTurn(queue.shift())
      .catch((error) => emit({ type: 'log', level: 'error', message: String(error?.message ?? error) }))
      .finally(() => { active = false; turnPromise = null; if (!closing) { setStatus(bridge.pending() ? 'waiting_for_answer' : 'idle'); pump(); } });
  };

  setStatus('idle');
  for await (const command of commands) {
    if (command?.type === 'user_message') {
      const text = typeof command.text === 'string' ? command.text : '';
      if (!text.trim()) { emit({ type: 'log', level: 'warn', message: 'ignored an empty user_message' }); continue; }
      const id = typeof command.id === 'string' && command.id ? command.id : `u-${++messageCount}`;
      emit({ type: 'user_message', id, text });
      queue.push(text); pump();
    } else if (command?.type === 'answer') {
      const answers = command.answers && typeof command.answers === 'object' && !Array.isArray(command.answers) ? command.answers : {};
      const response = typeof command.response === 'string' && command.response.trim() ? command.response : null;
      if (!bridge.answer(command.question_id, answers, response)) emit({ type: 'log', level: 'warn', message: `no open question with id ${command.question_id}` });
    } else if (command?.type === 'interrupt') {
      if (curState) curState.cancelled = true;
      try { current?.kill('SIGINT'); } catch { /* already gone */ }
    } else if (command?.type === 'set_model') {
      const next = typeof command.model === 'string' ? command.model.trim() : '';
      const split = splitModel(next, routes);
      if (!next || split.error === 'bare') emit({ type: 'log', level: 'warn', message: `ignored set_model without a <provider>/<model> model${next ? `: ${next}` : ''}` });
      else if (split.error === 'unrouted') emit({ type: 'log', level: 'warn', message: `ignored set_model ${next}: provider "${split.provider}" has no route` });
      else if (!split.name) emit({ type: 'log', level: 'warn', message: `ignored set_model ${next}: no model after the /` });
      else { emit({ type: 'model_changed', model: next, previous: currentModel }); currentModel = next; }
    } else if (command?.type === 'shutdown') break;
  }

  closing = true;
  bridge.cancelAll();
  if (current) {
    try { current.kill('SIGINT'); } catch { /* already gone */ }
    await Promise.race([turnPromise, new Promise((r) => setTimeout(r, graceMs))]);
    if (active) {
      try { current?.kill('SIGKILL'); } catch { /* already gone */ }
      await Promise.race([turnPromise, new Promise((r) => setTimeout(r, 2000))]);
    }
  } else await turnPromise;
  setStatus('exited');
  await bridge.close();
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

  const fail = (message) => { emit({ type: 'log', level: 'error', message }); emit({ type: 'status', state: 'error', detail: message }); process.exit(1); };
  const env = process.env;
  const { routes, warnings } = parseRoutes(env.COLONIZER_MODEL_ROUTES ?? '');
  for (const message of warnings) emit({ type: 'log', level: 'warn', message });
  const model = (env.COLONIZER_MODEL ?? '').trim();
  const smallExplicit = (env.COLONIZER_SMALL_MODEL ?? '').trim();
  const fatal = preflight(model, routes);
  if (fatal) fail(fatal);

  const moduleDir = dirname(fileURLToPath(import.meta.url));
  let lockText = '';
  try { lockText = readFileSync(join(moduleDir, 'opencode.lock'), 'utf8'); } catch { /* resolveOpencode reports the missing pin */ }
  let bin;
  try {
    bin = await resolveOpencode({ env, lockText, log: ({ level, message }) => emit({ type: 'log', level, message }) });
  } catch (error) { fail(`OpenCode binary: ${error?.message ?? error}`); }
  const instrPath = join(tmpdir(), `colonizer-opencode-instructions-${process.pid}.md`);
  writeFileSync(instrPath, `${INSTRUCTIONS}\n`);
  const makeEnv = (bridge, current = model) => ({
    ...env,
    OPENCODE_CONFIG_CONTENT: JSON.stringify({ ...opencodeConfig({ routes, model: current, smallModel: smallExplicit || current, mcp: { colonizer: { type: 'local', command: [process.execPath, join(moduleDir, 'mcp.mjs')], environment: { COLONIZER_BRIDGE_URL: bridge.url, COLONIZER_BRIDGE_TOKEN: bridge.token }, timeout: MCP_TIMEOUT_MS } } }), instructions: [instrPath] }),
    OPENCODE_DISABLE_MODELS_FETCH: '1', OPENCODE_DISABLE_AUTOUPDATE: '1', OPENCODE_DISABLE_DEFAULT_PLUGINS: '1', OPENCODE_DISABLE_LSP_DOWNLOAD: '1',
  });

  emit({ type: 'model_changed', model, previous: null });
  await runAgent({ commands, emit, bin, model, routes, makeEnv, findings: env.COLONIZER_FINDINGS === 'true' });
  process.exit(0);
}

const isEntrypoint = (() => { try { return realpathSync(process.argv[1]) === fileURLToPath(import.meta.url); } catch { return false; } })();

if (isEntrypoint) {
  main().catch((err) => {
    process.stderr.write(`${err?.stack ?? err}\n`);
    process.stdout.write(`${JSON.stringify({ type: 'status', state: 'exited', detail: String(err?.message ?? err) })}\n`);
    process.exit(1);
  });
}
