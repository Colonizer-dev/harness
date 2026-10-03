#!/usr/bin/env node
// Colonizer agent runner for xAI's Grok Build CLI (`grok`), headless: the runner contract of
// docs/protocol.md §2 (JSON-line commands on stdin, JSON-line protocol events on stdout). One
// `grok` child per turn; the first turn's `end` event carries the grok sessionId and every later
// turn resumes it with `-r`, so a colony is one continuous grok session. Questions go through the
// colonizer MCP server's ask_user tool (mcp.mjs, registered in the fresh GROK_HOME's config.toml)
// and a loopback bridge below. The pin lives in module.json; every flag and event field is cited
// from the upstream user guide in the README.

import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { gunzipSync } from 'node:zlib';

const here = dirname(fileURLToPath(import.meta.url));

// The named preflight problems (README, "Preflight"): each is the detail on the `status error`
// event and the prefix of the log that says how to fix it.
export const MISSING_CREDENTIAL = 'GROK_CREDENTIAL_MISSING';
export const WORKSPACE_UNTRUSTABLE = 'GROK_WORKSPACE_UNTRUSTABLE';
export const MISSING_BINARY = 'GROK_BINARY_MISSING';
export const VERSION_DRIFT = 'GROK_VERSION_DRIFT';
export const MODEL_PROVIDER = 'GROK_MODEL_PROVIDER';

// The wires a gateway route can speak (docs/protocol.md §6.5); a missing `wire` means the default.
const WIRES = new Set(['anthropic', 'openai']);
const HEADER_NAME = /^[A-Za-z0-9-]+$/;

/** COLONIZER_MODEL_ROUTES → validated routes, with warnings instead of throwing — the same wire
 * shape the hermes and pi runners parse, validated again here because a module is an independent
 * directory. `wire` is newer than the routes themselves: a route without one speaks the gateway's
 * default, anthropic. */
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
    const wire = entry?.wire ?? 'anthropic';
    if (!prefix.endsWith('/') || prefix.length < 2 || !/^https?:\/\//.test(baseUrl) || !WIRES.has(wire)) {
      warnings.push(`ignoring model route ${i}: needs a "<provider>/" prefix, an http(s) base_url and wire anthropic|openai`);
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
      wire,
    });
  });
  return { routes, warnings };
}

/** The pin, read from module.json so the manifest and this preflight cannot drift apart. */
export function readPin() {
  const pin = JSON.parse(readFileSync(join(here, 'module.json'), 'utf8'))?.requires?.pins?.grok;
  if (!pin?.version) throw new Error('module.json carries no grok pin');
  return pin;
}

/** Where the grok binary is by env alone: COLONIZER_GROK_BIN wins, else `grok` on the PATH. */
export function grokBin(env) {
  return String(env.COLONIZER_GROK_BIN ?? '').trim() || 'grok';
}

/** The lock row for this machine: unlike opencode there is no baseline build, so the two guest
 * architectures map one to one. */
export function archPlatform({ arch = process.arch } = {}) {
  if (arch === 'arm64') return 'linux-arm64';
  if (arch === 'x64') return 'linux-x64';
  return null;
}

/** Disk cache for the binary: the colony's /tmp is a small tmpfs, so the ~50–67 MB download and the
 * decompressed binary (136–163 MB) live under the cache dir instead (os.tmpdir() only when HOME is
 * unset). */
export function defaultCacheDir(env = process.env) {
  const base = env.XDG_CACHE_HOME || (env.HOME ? join(env.HOME, '.cache') : null);
  return base ? join(base, 'colonizer', 'grok') : join(tmpdir(), 'colonizer-grok');
}

/** The grok binary: COLONIZER_GROK_BIN, then PATH, then the pinned build for this arch — downloaded
 * from x.ai as a single gzip'd static ELF, sha256-checked before it is decompressed. The pin is read
 * back from module.json so the lock and the manifest cannot drift. */
export async function resolveGrok({ env = process.env, lockText, arch = process.arch, fetchImpl = fetch, cacheDir = defaultCacheDir(env), log = () => {}, version = readPin().version } = {}) {
  const explicit = String(env.COLONIZER_GROK_BIN ?? '').trim();
  if (explicit) return explicit;
  for (const dir of String(env.PATH ?? '').split(':')) if (dir && existsSync(join(dir, 'grok'))) return join(dir, 'grok');
  const platform = archPlatform({ arch });
  const row = String(lockText ?? '').split('\n').map((l) => l.trim().split(/\s+/)).filter((c) => c.length >= 6 && !c[0].startsWith('#')).map(([, v, p, , sha256, url]) => ({ version: v, platform: p, sha256, url })).find((r) => r.platform === platform && r.version === version);
  if (!row) throw new Error(`no pinned grok ${version} build for platform ${platform ?? arch}`);
  const dest = join(cacheDir, row.version, row.platform);
  const bin = join(dest, 'grok');
  if (existsSync(bin)) return bin;
  log({ level: 'info', message: `downloading grok ${row.version} (${row.platform})` });
  const res = await fetchImpl(row.url);
  if (!res?.ok) throw new Error(`grok download failed: HTTP ${res?.status ?? 'no response'}`);
  const bytes = Buffer.from(await res.arrayBuffer());
  if (createHash('sha256').update(bytes).digest('hex') !== row.sha256) throw new Error(`grok ${row.version} (${row.platform}) refused: sha256 mismatch`);
  const unpacked = gunzipSync(bytes); // one compressed ELF, not a tarball
  mkdirSync(dest, { recursive: true });
  const tmp = `${bin}.${process.pid}.tmp`;
  try {
    writeFileSync(tmp, unpacked);
    chmodSync(tmp, 0o755);
    renameSync(tmp, bin); // atomic: a half-written binary is never visible at `bin`
  } catch (error) {
    rmSync(tmp, { force: true }); // a partial 163 MB binary must not sit in the cache
    throw error;
  }
  return bin;
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

/** Whether folder trust could never gate this workspace and would auto-trust it instead: upstream's
 * trust store refuses a root of the filesystem root or the home directory (`is_unsafe_trust_root`),
 * and an unrecordable key resolves Trusted even headless with `GROK_FOLDER_TRUST=1` — so a colony
 * parked at either path would load its project-scope `.grok/` config in spite of the forced gate.
 * A checkout merely *inside* $HOME keys on the checkout (upstream `workspace_key` falls back to the
 * cwd when the git root is over-broad), so only the cwd itself refuses. Returns the offending root,
 * or null when the workspace is gateable. */
export function untrustableWorkspace(cwd, home) {
  const root = realpathSync(cwd);
  if (dirname(root) === root) return root; // the filesystem root keys itself
  try {
    if (home && realpathSync(home) === root) return root;
  } catch {
    // A home that does not exist cannot equal the workspace.
  }
  return null;
}

/** The checks that must pass before any grok process is spawned: fail loudly, not on a prompt.
 * Returns `{ problem, bin }` — the resolved binary path (`bin`) is what the turns must later spawn,
 * so it is resolved here, once, after the cheap env checks and before any download or version run. */
export async function preflight({ env, cwd = process.cwd(), spawnFn = spawn, pin = readPin(), lockText, log = () => {} } = {}) {
  // A model routed through the gateway needs no key of its own: the colony token in the route's
  // headers is the bearer credential (§6.5), so the check only gates direct api.x.ai runs.
  const routed = resolveModel(env.COLONIZER_MODEL, parseRoutes(env.COLONIZER_MODEL_ROUTES).routes).route;
  if (!String(env.XAI_API_KEY ?? '').trim() && !routed) {
    return {
      bin: null,
      problem: {
        code: MISSING_CREDENTIAL,
        message:
          'XAI_API_KEY is unset or empty, and the colony never runs browser OAuth (grok login). ' +
          'Add an xAI API key from console.x.ai as a colony secret named XAI_API_KEY for host api.x.ai, ' +
          'so the mothership injects it into this colony (README, "Credential story").',
      },
    };
  }
  const untrustable = untrustableWorkspace(cwd, env.HOME);
  if (untrustable) {
    return {
      bin: null,
      problem: {
        code: WORKSPACE_UNTRUSTABLE,
        message:
          `the workspace is "${untrustable}", which grok's folder trust auto-trusts instead of gating ` +
          '(a trust root of the home directory or the filesystem root can never be recorded in ' +
          'trusted_folders.toml), so project-scope .grok/ config would load in spite of GROK_FOLDER_TRUST=1. ' +
          'Run the colony from a dedicated worktree instead.',
      },
    };
  }
  let bin;
  try {
    bin = await resolveGrok({ env, lockText, log, version: pin.version });
  } catch (error) {
    return {
      bin: null,
      problem: { code: MISSING_BINARY, message: `the pinned grok CLI could not be fetched: ${error?.message ?? error}. Install the pinned version: curl -fsSL ${pin.install} | bash -s ${pin.version}` },
    };
  }
  const text = await versionOf(bin, spawnFn);
  if (text === undefined) {
    return { bin, problem: { code: MISSING_BINARY, message: `the grok CLI was not found at "${bin}". Install the pinned version: curl -fsSL ${pin.install} | bash -s ${pin.version}` } };
  }
  const found = parseVersion(text);
  if (found !== pin.version) {
    return {
      bin,
      problem: {
        code: VERSION_DRIFT,
        message:
          `grok --version printed "${String(text).trim()}" (parsed ${found ?? 'nothing'}) instead of the pinned ` +
          `${pin.version} (SOURCE_REV ${pin.source_rev}). Install the pinned version: curl -fsSL ${pin.install} | bash -s ${pin.version}`,
      },
    };
  }
  return { bin, problem: null };
}

/** The `-m` value for a model setting, plus the gateway route it rides when one applies. A bare
 * xAI model id, or `xai-grok/<model>` with no `xai-grok/` route configured, goes straight to
 * api.x.ai as before; a prefix a route carries rides that route through the provider gateway when
 * its wire is `openai` — the only wire the gateway serves grok's OpenAI base URL on (§6.5). An
 * anthropic-wire route, or a prefix nobody configured, is refused by name. */
export function resolveModel(spec, routes = []) {
  const value = String(spec ?? '').trim();
  if (!value) return {};
  const slash = value.indexOf('/');
  if (slash < 1) return { model: value };
  const prefix = value.slice(0, slash + 1);
  const route = routes.find((r) => r.prefix === prefix);
  if (route) {
    if (route.wire !== 'openai') {
      return { error: `${MODEL_PROVIDER}: "${value}" names provider ${route.provider}, whose gateway route speaks the anthropic wire; grok can only ride openai-wire routes (Settings → Model providers)` };
    }
    return { model: value.slice(slash + 1), route };
  }
  if (prefix === 'xai-grok/') return { model: value.slice(slash + 1) };
  return { error: `${MODEL_PROVIDER}: "${value}" names a provider with no gateway route; add it under Settings → Model providers, or use xai-grok/<model>` };
}

/** Loopback bridge to mcp.mjs: finding_file, memory_propose, loop_next and loop_stop arrive here
 * and leave the colony as protocol events (docs/protocol.md §6.6, §6.2), the way the opencode
 * module's bridge does. An ask_user call parks here instead: the HTTP response is held until the
 * matching `answer` command resolves it with {answers, response}, the question card having gone
 * out as a `question` event (§2). The loop delay was clamped in mcp.mjs; the bridge only validates
 * the shape it must not emit malformed (§2's delay_minutes is an integer). */
export async function createBridge({ emit, findings = false, setStatus = () => {}, isWorking = () => false, token = randomBytes(16).toString('hex') }) {
  let count = 0;
  const pending = new Map();
  const server = createServer((req, res) => {
    const reply = (status, payload) => { if (!res.destroyed) { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(payload)); } };
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
          emit({ type: 'memory_proposal', origin: 'orchestrator', scope, title: msg.title, content: msg.content, tags: Array.isArray(msg.tags) ? msg.tags.map(String) : [], ...(typeof msg.kind === 'string' ? { kind: msg.kind } : {}), ...(Number.isFinite(msg.confidence) ? { confidence: msg.confidence } : {}) });
          reply(200, { ok: true });
        }
      } else if (req.url === '/loop_next') {
        const minutes = Number(msg.delay_minutes);
        if (!Number.isFinite(minutes) || minutes < 1) reply(200, { error: 'loop_next needs delay_minutes: a number of minutes from now' });
        else if (typeof msg.reason !== 'string' || !msg.reason.trim()) reply(200, { error: 'loop_next needs a reason: what the next run should find or do' });
        else {
          emit({ type: 'loop_next', delay_minutes: Math.round(minutes), reason: msg.reason });
          reply(200, { ok: true });
        }
      } else if (req.url === '/loop_stop') {
        if (typeof msg.reason !== 'string' || !msg.reason.trim()) reply(200, { error: 'loop_stop needs a reason: why the loop should stop' });
        else {
          emit({ type: 'loop_stop', reason: msg.reason });
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

/** The `$GROK_HOME/config.toml` body that registers the colonizer MCP server (mcp.mjs) with grok: the
 * upstream table is `mcp_servers.<id>` with `command`, `args` and `env` (26-config-reference.md), and
 * `--always-approve` auto-approves its tool calls. The values ride as JSON, which is valid TOML for
 * the strings, the array and the inline table; a `tool_timeout_sec` override is unneeded (the 6000 s
 * default covers a wait's 1800 s cap). Findings, memory and the loop switches ride through so
 * mcp.mjs can gate its tool list the way the mothership gated the runner env. */
export function configToml({ url, token, env }) {
  const envEntries = Object.entries({
    COLONIZER_BRIDGE_URL: url,
    COLONIZER_BRIDGE_TOKEN: token,
    COLONIZER_FINDINGS: env.COLONIZER_FINDINGS, // undefined drops out of the inline table
    COLONIZER_MEMORY_DIR: env.COLONIZER_MEMORY_DIR,
    COLONIZER_LOOP: env.COLONIZER_LOOP, // a loop colony's pacing tools; SELF_PACED gates loop_next
    COLONIZER_LOOP_SELF_PACED: env.COLONIZER_LOOP_SELF_PACED,
  }).filter(([, value]) => value !== undefined);
  const json = JSON.stringify;
  return [
    '[mcp_servers.colonizer]',
    `command = ${json(process.execPath)}`,
    `args = ${json([join(here, 'mcp.mjs')])}`,
    `env = { ${envEntries.map(([key, value]) => `${key} = ${json(value)}`).join(', ')} }`,
  ].join('\n');
}

/** One headless grok turn (14-headless-mode.md), with the nesting decisions from the README applied.
 * `disabledTools` carries the module.json `disabled_tools` names, passed through grok's own headless
 * denylist; the harness validated each against the manifest's "x-known-tools" at boot. */
export function turnArgs({ promptFile, model, sessionId, disabledTools = [] }) {
  const args = [
    '--prompt-file', promptFile,
    '--output-format', 'streaming-json',
    '--always-approve', // headless cannot answer a permission prompt; the microVM is the boundary
    '--sandbox', 'off', // grok's own OS sandbox stays off; the microVM is the colony's boundary
    '--disable-web-search', // the web-search backend's host is unverified, and egress denies it anyway
    '--no-auto-update',
  ];
  if (disabledTools.length) args.push('--disallowed-tools', disabledTools.join(',')); // the headless denylist (14-headless-mode.md)
  if (model) args.push('-m', model);
  if (sessionId) args.push('-r', sessionId); // resume: a colony is one continuous grok session
  return args;
}

/** The child's environment. A fresh GROK_HOME is the one nesting lever the docs verify: grok's
 * config, cached OAuth token, skills and MCP definitions all live under it (README "Nesting"). A
 * routed model moves the model host to the gateway: GROK_MODELS_BASE_URL is the route's OpenAI
 * passthrough and grok then sends the API key as `Authorization: Bearer` (Vercel AI Gateway docs
 * for Grok Build), so the key is the colony token from the route's headers and the direct api.x.ai
 * key comes out of the child env — the placeholder a colony holds must never reach the gateway. */
export function childEnv(env, home, route = null) {
  const mapped = { ...env };
  if (route) {
    delete mapped.XAI_API_KEY;
    mapped.GROK_MODELS_BASE_URL = `${route.base_url.replace(/\/+$/, '')}/v1`;
    if (route.headers['x-colonizer-colony']) mapped.GROK_CODE_XAI_API_KEY = route.headers['x-colonizer-colony'];
  }
  return {
    ...mapped,
    BROWSER: '/bin/false', // belt-and-braces: nothing may open a browser, and this runner never runs grok login
    GROK_HOME: home,
    // The folder-trust gate forced on (env beats a `[folder_trust] enabled` kill-switch in any
    // config): headless, with a fresh GROK_HOME's empty trust store, the workspace resolves
    // untrusted and grok skips project-scope .grok/ MCP servers, plugins, hooks and skills
    // (and project LSP/instructions). '0' would switch the gate off, so an inherited host
    // value must never pass through — the runner never passes --trust instead.
    GROK_FOLDER_TRUST: '1',
    GROK_MEMORY: '0', // no cross-session memory (05-configuration.md)
    GROK_TELEMETRY_ENABLED: '0', // product analytics off (05-configuration.md)
    GROK_DISABLE_AUTOUPDATER: '1', // no update checks inside a colony (14-headless-mode.md)
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
export function startTurn({ prompt, model, sessionId, messageId, env, home, bin = grokBin(env), emit, spawnFn = spawn, totals, disabledTools = [], route = null }) {
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
    // colonizer__ask_user calls are question traffic, not work: §2 says a question is never also a
    // tool_call/tool_result, so the call ids are remembered only to drop their updates. The
    // colonizer server's other tools (findings, memory, the loop tools) still stream.
    const askCalls = new Set();
    child = spawnFn(bin, turnArgs({ promptFile, model, sessionId, disabledTools }), { env: childEnv(env, home, route), stdio: ['ignore', 'pipe', 'pipe'] });
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
          if (name === 'colonizer__ask_user') {
            if (event.toolCallId != null) askCalls.add(String(event.toolCallId));
            break; // its outcome is the question/question_answered flow
          }
          emit({ type: 'tool_call', message_id: messageId, tool_call_id: String(event.toolCallId ?? ''), name, input: plainObject(event.rawInput) });
          break;
        }
        case 'tool_call_update': {
          if (askCalls.has(String(event.toolCallId ?? ''))) break; // a question is never a tool_result
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
  const { routes, warnings } = parseRoutes(env.COLONIZER_MODEL_ROUTES);
  for (const message of warnings) emit({ type: 'log', level: 'warn', message });
  let lockText = '';
  try { lockText = readFileSync(join(here, 'grok.lock'), 'utf8'); } catch { /* resolveGrok reports the missing pin */ }
  const { problem, bin } = await preflight({ env, spawnFn, lockText, log: ({ level, message }) => emit({ type: 'log', level, message }) });
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
  // Harness-level tool switch (module.json `disabled_tools`): the names ride every turn as grok's
  // own `--disallowed-tools` denylist. The harness validated them against the manifest's
  // "x-known-tools" at boot; here they are only trimmed, with empties dropped.
  const disabledTools = String(env.COLONIZER_DISABLED_TOOLS ?? '')
    .split(',')
    .map((tool) => tool.trim())
    .filter(Boolean);

  // One bridge for the runner's life; the colonizer MCP server grok spawns points at it through the
  // config.toml the runner wrote into its fresh GROK_HOME (the address is fixed, so one write). An
  // ask_user call parks on it (createBridge), so question routing needs nothing beyond the
  // registration that was already unconditional: ask_user is on whenever the runner is.
  const bridge = await createBridge({ emit, findings: env.COLONIZER_FINDINGS === 'true', setStatus, isWorking: () => turn !== null });
  await writeFile(join(home, 'config.toml'), configToml({ url: bridge.url, token: bridge.token, env }), 'utf8');

  const pump = async () => {
    if (pumping) return;
    pumping = true;
    try {
      while (pending.length) {
        const message = pending.shift();
        setStatus('working');
        const resolved = resolveModel(modelSpec, routes);
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
        turn = startTurn({ prompt: message.text, model: resolved.model, sessionId, messageId: `msg-${n}`, env, home, bin, emit, spawnFn, totals, disabledTools, route: resolved.route ?? null });
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
        const resolved = resolveModel(command.model, routes);
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

  bridge.cancelAll(); // a parked ask is released with the runner above; the orphaned mcp.mjs drains
  if (turn) {
    turn.interrupt();
    await Promise.race([turn.done, sleep(3000)]);
  }
  await bridge.close();
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
