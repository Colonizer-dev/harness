#!/usr/bin/env node
// Boots one real colony end to end and asserts it comes back `no_changes`: a real mothership
// (`colonizer serve`), a real microVM through the vendored msb, real agentd, the real claude-code
// runner module and the real Claude Code CLI. The only stand-ins are what a pull request must never
// let CI touch: the model (a stub Anthropic-wire server in this process), GitHub (a scratch git
// repository on disk plus a stub `gh`) and the Claude credential (a dummy token; the gate only
// checks that one is saved).
//
//   node scripts/colony-e2e.mjs [--work DIR] [--timeout-mins N] [--keep]
//
// The colonizer binary is target/release/colonizer and the assets (msb, guest runtimes, the agent
// module) are dist/, both as scripts/install.sh leaves them in a checkout. The work dir holds
// everything else; it is kept on failure (CI uploads it) and removed on success unless --keep is
// passed. home/.microsandbox, the pulled colony image, survives both, so a reused --work dir does
// not re-download it.
import { spawn, spawnSync } from 'node:child_process';
import { chmodSync, closeSync, existsSync, mkdirSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = 'e2e/colony-check';
const MODEL = 'stub/colony-e2e';
const TERMINAL = new Set(['pr_opened', 'merged', 'closed', 'no_changes', 'stopped', 'failed']);
// `sk-ant-api…` rather than an OAuth token on purpose: the mothership maps the prefix to
// ANTHROPIC_API_KEY, so the in-VM Claude Code never tries an OAuth refresh against the real
// api.anthropic.com, which the colony's network fence blocks.
const CLAUDE_TOKEN = 'sk-ant-api-e2e-stub-not-a-real-key';

// --------------------------------------------------------------------- the stub model's replies

/** The one file the orchestrator is allowed to write itself (runner.mjs, ENFORCE_PROMPT_APPEND). */
export const PR_PATH = '/harness/out/pr.md';
export const PR_TITLE = 'No changes needed';

/**
 * The stub's answer to one Anthropic-wire /v1/messages body — pure, so the policy is tested apart
 * from the server (scripts/test/colony-e2e.test.mjs). The first orchestrator request offers Write
 * with no tool_result yet in the conversation: answer with the one Write call that makes the turn
 * publishable — the PR description at /harness/out/pr.md, outside the worktree, so autopilot
 * publishes, finds nothing staged and no commits ahead, and ends `no_changes`. Every later request
 * (a tool_result is back, or Write is not offered, e.g. a background title call) gets plain text
 * and `end_turn`. AskUserQuestion is never offered, so the colony never waits on a human.
 */
export function stubReply(body) {
  const tools = Array.isArray(body?.tools) ? body.tools.map((t) => t?.name).filter(Boolean) : [];
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const hasToolResult = messages.some((m) => Array.isArray(m?.content) && m.content.some((b) => b?.type === 'tool_result'));
  if (tools.includes('Write') && !hasToolResult) {
    return {
      stop_reason: 'tool_use',
      content: [
        {
          type: 'tool_use',
          id: 'toolu_e2e_pr',
          name: 'Write',
          input: { file_path: PR_PATH, content: `${PR_TITLE}\n\nChecked the repository; no change was needed for this task.` },
        },
      ],
    };
  }
  return {
    stop_reason: 'end_turn',
    content: [{ type: 'text', text: 'The repository needs no change for this task, so there is nothing to hand over beyond the PR description.' }],
  };
}

/** One streaming reply as the SSE (`event`, `data`) frames the Claude Code CLI parses, in order. */
export function replyFrames(model, reply) {
  const id = `msg_e2e_${Date.now().toString(36)}`;
  const frames = [
    ['message_start', { type: 'message_start', message: { id, type: 'message', role: 'assistant', model, content: [], stop_reason: null, stop_sequence: null } }],
  ];
  reply.content.forEach((block, index) => {
    if (block.type === 'text') {
      frames.push(['content_block_start', { type: 'content_block_start', index, content_block: { type: 'text', text: '' } }]);
      frames.push(['content_block_delta', { type: 'content_block_delta', index, delta: { type: 'text_delta', text: block.text } }]);
    } else {
      frames.push(['content_block_start', { type: 'content_block_start', index, content_block: { type: 'tool_use', id: block.id, name: block.name, input: {} } }]);
      frames.push(['content_block_delta', { type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } }]);
    }
    frames.push(['content_block_stop', { type: 'content_block_stop', index }]);
  });
  frames.push(['message_delta', { type: 'message_delta', delta: { stop_reason: reply.stop_reason, stop_sequence: null }, usage: { output_tokens: 16 } }]);
  frames.push(['message_stop', { type: 'message_stop' }]);
  return frames;
}

/** A listening stub model server. Every request is logged to `requests` for the failure report. */
async function startStubModel() {
  const requests = [];
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = Buffer.concat(chunks).toString('utf8');
    const path = req.url?.split('?')[0] ?? '';
    const entry = { method: req.method, path, replied: null };
    requests.push(entry);
    const send = (status, payload) => {
      res.writeHead(status, { 'content-type': 'application/json' });
      res.end(JSON.stringify(payload));
    };
    if (path === '/v1/models') {
      entry.replied = 'models';
      return send(200, { data: [{ id: 'colony-e2e', display_name: 'Colony e2e stub' }] });
    }
    if (path.endsWith('/messages/count_tokens')) {
      entry.replied = 'count_tokens 404'; // 404 is the agreed "no estimator here"; the in-VM router answers locally (router.mjs)
      return send(404, { type: 'error', error: { type: 'not_found_error', message: 'no token estimator' } });
    }
    if (path !== '/v1/messages' || req.method !== 'POST') {
      entry.replied = 'unexpected path';
      return send(404, { type: 'error', error: { type: 'not_found_error', message: `stub model: unexpected ${req.method} ${path}` } });
    }
    let parsed = null;
    try {
      parsed = JSON.parse(body);
    } catch {
      // A body that is not JSON gets the plain-text reply; nothing in a colony sends one.
    }
    const reply = stubReply(parsed);
    entry.replied = reply.stop_reason === 'tool_use' ? 'Write pr.md' : 'text end_turn';
    // Every caller in a colony streams — the Claude Code CLI always does — so the reply is always SSE.
    const frames = replyFrames(parsed.model ?? 'colony-e2e', reply);
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    for (const [event, data] of frames) res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
    res.end();
  });
  await new Promise((resolvePromise, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolvePromise);
  });
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    port: server.address().port,
    requests,
    close: () =>
      new Promise((r) => {
        server.close(() => r());
        server.closeAllConnections?.();
      }),
  };
}

// ------------------------------------------------------------------------------ the run itself

const parseArgs = (argv) => {
  const out = { work: null, timeoutMins: 20, keep: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const next = () => argv[++i];
    if (arg === '--work') out.work = next();
    else if (arg === '--timeout-mins') out.timeoutMins = Number(next());
    else if (arg === '--keep') out.keep = true;
    else throw new Error(`unknown argument: ${arg}`);
  }
  if (!Number.isInteger(out.timeoutMins) || out.timeoutMins < 1) throw new Error('--timeout-mins must be a positive integer');
  return out;
};

/** A free loopback port, picked by listening and closing: the mothership refuses a bind someone holds. */
const freePort = () =>
  new Promise((resolvePromise, reject) => {
    const probe = createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address();
      probe.close(() => resolvePromise(port));
    });
  });

const run = (command, args, opts = {}) => {
  const done = spawnSync(command, args, { encoding: 'utf8', ...opts });
  if (done.status !== 0) throw new Error(`${command} ${args.join(' ')} failed (${done.status}): ${done.stderr ?? ''}`);
  return done.stdout;
};

/** An environment copy with every GIT_* variable dropped: a surrounding GIT_DIR or GIT_WORK_TREE
 * (this script runs inside colonies and CI steps that set them) would hijack the scratch repo and
 * the mothership's own git alike. GIT_CONFIG_GLOBAL is set back explicitly by both callers. */
const scrubGit = (env) => {
  const out = { ...env };
  for (const key of Object.keys(out)) {
    if (key.startsWith('GIT_')) delete out[key];
  }
  return out;
};

/** The tail of a JSONL log as readable lines, for the failure report. */
const tailLines = (file, lines = 40) => {
  if (!existsSync(file)) return `(no ${file})`;
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter(Boolean)
    .slice(-lines)
    .map((line) => {
      try {
        const e = JSON.parse(line);
        return `${e.ts ?? ''} ${e.level ?? ''} ${e.message ?? line}`.trim().slice(0, 400);
      } catch {
        return line.slice(0, 400);
      }
    })
    .join('\n');
};

export async function main(argv) {
  const args = parseArgs(argv);
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const colonizer = join(root, 'target', 'release', 'colonizer');
  const dist = join(root, 'dist');
  if (!existsSync(colonizer)) throw new Error(`no colonizer binary at ${colonizer}; build it with cargo build --release -p colonizer-harness`);
  const work = resolve(args.work ?? join(tmpdir(), `colony-e2e-${Date.now().toString(36)}`));
  mkdirSync(work, { recursive: true });
  const log = console.log;

  // Everything per-run is wiped; home/ is kept, so a reused work dir keeps the msb image cache.
  const perRun = ['scratch', 'bin', 'config', 'data', 'home-run', 'seed'];
  for (const part of perRun) rmSync(join(work, part), { recursive: true, force: true });
  // <work>/scratch stands in for github.com: it holds e2e/colony-check.git at owner/name.git.
  const scratch = join(work, 'scratch');
  const home = join(work, 'home');
  const configDir = join(work, 'config');
  const dataDir = join(work, 'data');
  for (const dir of [join(scratch, 'e2e'), join(work, 'bin'), home, configDir, dataDir, join(work, 'home-run')]) mkdirSync(dir, { recursive: true });

  // The scratch repository the mothership "clones from GitHub": a bare repo with one commit on
  // main. GIT_CONFIG_GLOBAL points the driver's git and the mothership's own git here, with
  // insteadOf rewriting https://github.com/ onto file://<work>/scratch/, so the hardcoded clone URL
  // resolves to it.
  const gitconfig = join(work, 'gitconfig');
  const gitEnv = scrubGit(process.env);
  gitEnv.GIT_CONFIG_GLOBAL = gitconfig;
  writeFileSync(gitconfig, `[url "file://${scratch}/"]\n\tinsteadOf = https://github.com/\n[user]\n\tname = e2e\n\temail = e2e@example.invalid\n`);
  const seed = join(work, 'seed');
  run('git', ['-c', 'init.defaultBranch=main', 'init', '--quiet', seed], { env: gitEnv });
  writeFileSync(join(seed, 'README.md'), '# colony-check\n\nA scratch repository for the end-to-end colony test.\n');
  run('git', ['add', 'README.md'], { cwd: seed, env: gitEnv });
  run('git', ['commit', '--quiet', '-m', 'initial state'], { cwd: seed, env: gitEnv });
  run('git', ['clone', '--quiet', '--bare', seed, join(scratch, 'e2e', 'colony-check.git')], { env: gitEnv });
  rmSync(seed, { recursive: true, force: true });

  // The stub `gh`, first on the serve process's PATH. A no-issue colony calls `--version`, the
  // viewer probe `gh api user` (whose failure it tolerates), and `gh api repos/{repo} --jq
  // .default_branch`; anything else fails loudly and is logged, so an unexpected GitHub dependency
  // shows up in the report instead of hanging the boot retry loop.
  const ghStub = join(work, 'bin', 'gh');
  const ghLog = join(work, 'gh-calls.log');
  writeFileSync(
    ghStub,
    [
      '#!/bin/sh',
      `echo "$*" >> ${JSON.stringify(ghLog)}`,
      'case "$*" in',
      '  --version) echo "gh version 2.99.0 (e2e stub)"; exit 0;;',
      '  "api repos/"*" --jq .default_branch") echo main; exit 0;;',
      'esac',
      'echo "gh-e2e-stub: unexpected call: $*" >&2',
      'exit 1',
      '',
    ].join('\n'),
  );
  chmodSync(ghStub, 0o755);

  log(`work dir ${work}`);
  const started = Date.now();
  const stub = await startStubModel();
  log(`stub model on ${stub.url}`);

  const bindPort = await freePort();
  const gatewayPort = await freePort();
  // Scrubbed, like the driver's git: the mothership must not see a real GitHub token (the stub gh
  // is the only GitHub), a Claude credential of the machine it runs on, or a hijacking GIT_DIR.
  const serveEnv = scrubGit(process.env);
  for (const key of ['GH_TOKEN', 'GITHUB_TOKEN', 'ANTHROPIC_API_KEY', 'CLAUDE_CODE_OAUTH_TOKEN', 'COLONIZER_MSB', 'COLONIZER_HOME']) delete serveEnv[key];
  Object.assign(serveEnv, {
    HOME: home,
    PATH: `${join(work, 'bin')}:${serveEnv.PATH ?? ''}`,
    GIT_CONFIG_GLOBAL: gitconfig,
    COLONIZER_BIND: `127.0.0.1:${bindPort}`,
    COLONIZER_GATEWAY_BIND: `127.0.0.1:${gatewayPort}`,
    COLONIZER_DATA_DIR: dataDir,
    COLONIZER_CONFIG_DIR: configDir,
    COLONIZER_HOME: dist,
    COLONIZER_NO_BROWSER: '1',
    // Update checks go to the stub, which 404s every unknown path, rather than to a real releases
    // feed; DO_NOT_TRACK keeps the local-run telemetry notice out of the log as CI's CI=true does.
    COLONIZER_RELEASES_URL: `http://127.0.0.1:${stub.port}/none`,
    DO_NOT_TRACK: '1',
    XDG_RUNTIME_DIR: join(work, 'home-run'),
  });
  const serveLog = join(work, 'serve.log');
  const serveOut = openSync(serveLog, 'a');
  const serve = spawn(colonizer, [], { env: serveEnv, cwd: work, stdio: ['ignore', serveOut, serveOut] });
  log(`colonizer serve (pid ${serve.pid}) on 127.0.0.1:${bindPort}, log ${serveLog}`);

  let sessionId = null;
  const api = (method, path, body) => {
    const token = readFileSync(join(configDir, 'api-token'), 'utf8').trim();
    return fetch(`http://127.0.0.1:${bindPort}${path}`, {
      method,
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      // A mothership that accepts the connection but never answers must not hang the driver past
      // its own deadlines; the boot-wait loop already treats any rejection, abort included, as
      // "not up yet" and retries.
      signal: AbortSignal.timeout(15_000),
    });
  };

  let failure = null;
  try {
    // Up when the token exists and /api/status answers; 120s covers a cold first boot.
    const tokenFile = join(configDir, 'api-token');
    let up = false;
    for (let i = 0; i < 240 && !up; i++) {
      if (existsSync(tokenFile)) {
        try {
          up = (await api('GET', '/api/status')).ok;
        } catch {
          // not listening yet
        }
      }
      if (!up) await new Promise((r) => setTimeout(r, 500));
    }
    if (!up) throw new Error('the mothership API did not come up within 120s');
    log('mothership API is up');

    // A dummy Claude credential: the claude-code module refuses to launch without one, and only the
    // sk-ant- prefix is checked — the value never reaches a real Anthropic endpoint.
    await needOk(api('POST', '/api/settings/claude-token', { token: CLAUDE_TOKEN }), 'saving the dummy Claude token');
    // The stub as the only provider, so every model call — orchestrator, subagent, background — is
    // provider-prefixed and routed through the mothership's gateway rather than to Anthropic.
    await needOk(api('PUT', '/api/providers/stub', { name: 'Stub', base_url: stub.url, auth: 'none', wire: 'anthropic', preset: 'custom' }), 'registering the stub provider');
    // `plugins: ''` on purpose: skillsets default on (archify), and this run stages none.
    await needOk(api('PUT', '/api/modules/agent', { provider: 'claude-code', enabled: true, settings: { model: MODEL, subagent_model: MODEL, background_model: MODEL, plugins: '' } }), 'pointing the agent module at the stub');
    // No mesh: the default headscale provider would start headscale and wait 120s for a mesh join.
    await needOk(api('PUT', '/api/modules/mesh', { provider: 'none', enabled: true, settings: {} }), 'switching the mesh off');
    log('mothership configured (stub provider, no mesh)');

    const created = await api('POST', '/api/sessions', {
      repo: REPO,
      autopilot: true,
      instructions:
        'Decide whether the repository needs a change for this task. If it does, make it; if not, write your pull request description to /harness/out/pr.md and finish your turn.',
    });
    if (!created.ok) throw new Error(`creating the colony failed: ${created.status} ${await created.text()}`);
    const session = await created.json();
    sessionId = session.id;
    log(`colony ${sessionId} created on ${REPO}; waiting for a terminal status`);

    let last = '';
    const deadline = Date.now() + args.timeoutMins * 60_000;
    let current = session;
    while (Date.now() < deadline) {
      if (serve.exitCode !== null) throw new Error(`the mothership exited early with code ${serve.exitCode}`);
      const fresh = await api('GET', `/api/sessions/${sessionId}`);
      if (!fresh.ok) throw new Error(`reading the colony failed: ${fresh.status} ${await fresh.text()}`);
      current = await fresh.json();
      if (current.status !== last) {
        log(`status: ${current.status}${current.error ? ` (${current.error})` : ''}`);
        last = current.status;
      }
      if (TERMINAL.has(current.status)) break;
      await new Promise((r) => setTimeout(r, 2000));
    }
    if (!TERMINAL.has(current.status)) {
      failure = `the colony did not finish within ${args.timeoutMins} minutes (status ${current.status})`;
    } else if (current.status !== 'no_changes') {
      failure = `the colony ended as ${current.status}, not no_changes${current.error ? `: ${current.error}` : ''}`;
    } else {
      log(`colony reached no_changes in ${Math.round((Date.now() - started) / 1000)}s; the stub model answered ${stub.requests.length} requests`);
    }
  } catch (e) {
    failure = String(e?.message ?? e);
  }

  if (failure) {
    console.log(`\n=== colony-e2e FAILED: ${failure}\n`);
    try {
      if (sessionId) {
        const fresh = await api('GET', `/api/sessions/${sessionId}`);
        console.log(`--- session ${sessionId}\n${JSON.stringify(await fresh.json(), null, 2)}`);
        const dir = join(dataDir, 'sessions', sessionId);
        console.log(`--- harness log (tail)\n${tailLines(join(dir, 'harness.jsonl'))}`);
        console.log(`--- agent events (tail)\n${tailLines(join(dir, 'events.jsonl'))}`);
      }
    } catch {
      // The report is best effort; the original failure is what matters.
    }
    console.log(`--- stub model requests (${stub.requests.length})\n${stub.requests.map((r) => `${r.method} ${r.path} -> ${r.replied}`).join('\n') || '(none reached the stub)'}`);
    console.log(`--- gh calls\n${existsSync(ghLog) ? readFileSync(ghLog, 'utf8') : '(none)'}`);
    console.log(`--- mothership log (tail)\n${tailLines(serveLog, 60)}`);
  }

  // Tear down whether the run passed or not. Stop first: a live colony's microVM is detached and
  // outlives the mothership (crates/colonizer/src/main.rs), and only stop's `msb rm` removes it.
  // Then forget the colony (its worktree), then the mothership and the stub. Nothing else survives.
  try {
    if (sessionId) {
      await api('POST', `/api/sessions/${sessionId}/stop`, {}).catch(() => {});
      await api('DELETE', `/api/sessions/${sessionId}`);
    }
  } catch {
    // The mothership may already be gone; its colonies are too.
  }
  serve.kill('SIGTERM');
  if (!(await exited(serve, 5000))) serve.kill('SIGKILL');
  await stub.close();
  closeSync(serveOut);

  if (failure) {
    console.log(`work dir kept for inspection: ${work}`);
    process.exitCode = 1;
    return;
  }
  if (args.keep) console.log(`work dir kept: ${work}`);
  else {
    // Keep home/ (the msb image cache, gigabytes) so a reused --work dir boots fast; drop the rest.
    for (const part of [...perRun, 'serve.log', 'gh-calls.log', 'gitconfig']) rmSync(join(work, part), { recursive: true, force: true });
  }
}

const needOk = async (call, what) => {
  const response = await call;
  const body = await response.text(); // always drained, so the socket is not left half-read
  if (!response.ok) throw new Error(`${what} failed: ${response.status} ${body}`);
};

const exited = (child, ms) =>
  new Promise((resolvePromise) => {
    if (child.exitCode !== null) return resolvePromise(true);
    const timer = setTimeout(() => resolvePromise(false), ms);
    child.once('exit', () => {
      clearTimeout(timer);
      resolvePromise(true);
    });
  });

try {
  if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
    main(process.argv.slice(2)).catch((err) => {
      console.error(err?.stack ?? err);
      process.exit(1);
    });
  }
} catch {
  // not the entry point; imported for the stub policy's tests
}
