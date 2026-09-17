import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { HEADROOM_ENV, headroomArgs, startHeadroom } from '../headroom.mjs';

/** A bundle whose "python3" is a shell script: records how it was started, then behaves as told. */
function fakeBundle(root, behaviour) {
  const bin = join(root, 'bundle', 'python', 'bin');
  mkdirSync(bin, { recursive: true });
  const record = join(root, 'started.json');
  const serve = `require('fs').writeFileSync(${JSON.stringify(record)}, JSON.stringify({ argv: process.argv.slice(1), env: process.env })); `;
  const scripts = {
    healthy: `${serve}const port = Number(process.argv[process.argv.indexOf('--port') + 1]); require('http').createServer((q, r) => r.end('ok')).listen(port, '127.0.0.1');`,
    crashes: `${serve}console.error('ModuleNotFoundError: no module named headroom'); process.exit(3);`,
    silent: `${serve}setInterval(() => {}, 1000);`,
  };
  const python = join(bin, 'python3');
  writeFileSync(python, `#!/bin/sh\nexec node -e ${JSON.stringify(scripts[behaviour])} -- "$@"\n`);
  chmodSync(python, 0o755);
  return { dir: join(root, 'bundle'), record };
}

const collect = () => {
  const logs = [];
  return { logs, log: (entry) => logs.push(entry) };
};

test('headroom arguments keep the cache off, write nothing, and point at the upstream when there is one', () => {
  assert.deepEqual(headroomArgs(8787), ['-m', 'headroom.cli', 'proxy', '--host', '127.0.0.1', '--port', '8787', '--stateless', '--no-cache']);
  assert.deepEqual(headroomArgs(8787, 'http://127.0.0.1:9000').slice(-2), ['--anthropic-api-url', 'http://127.0.0.1:9000']);
  assert.equal(HEADROOM_ENV.HEADROOM_OFFLINE, '1');
  assert.equal(HEADROOM_ENV.LITELLM_LOCAL_MODEL_COST_MAP, 'True');
});

test('without a mounted bundle, Headroom is skipped with a warning', async () => {
  const { logs, log } = collect();
  assert.equal(await startHeadroom({ dir: join(tmpdir(), 'no-such-headroom'), log }), null);
  assert.equal(logs.length, 1);
  assert.equal(logs[0].level, 'warn');
});

test('a healthy Headroom is started on loopback with only the environment it needs, and closes', async () => {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-headroom-'));
  try {
    const { dir, record } = fakeBundle(root, 'healthy');
    const { logs, log } = collect();
    const env = {
      PATH: process.env.PATH,
      CLAUDE_CODE_OAUTH_TOKEN: 'placeholder',
      DEEPSEEK_API_KEY: 'secret',
      COLONIZER_HEADROOM_HOME: join(root, 'home'),
      SSL_CERT_FILE: '/etc/ssl/certs/ca-certificates.crt',
      REQUESTS_CA_BUNDLE: '/etc/ssl/certs/ca-certificates.crt',
    };
    const headroom = await startHeadroom({ env, dir, upstream: 'http://127.0.0.1:9000', log, timeoutMs: 20_000 });
    assert.ok(headroom, JSON.stringify(logs));
    assert.match(headroom.url, /^http:\/\/127\.0\.0\.1:\d+$/);
    assert.equal((await fetch(`${headroom.url}/health`)).status, 200);

    const started = JSON.parse(readFileSync(record, 'utf8'));
    assert.deepEqual(started.argv.slice(-9), ['proxy', '--host', '127.0.0.1', '--port', headroom.url.split(':').pop(), '--stateless', '--no-cache', '--anthropic-api-url', 'http://127.0.0.1:9000']);
    for (const [key, value] of Object.entries(HEADROOM_ENV)) assert.equal(started.env[key], value, key);
    assert.equal(started.env.CLAUDE_CODE_OAUTH_TOKEN, undefined, 'no Claude credential, not even the placeholder');
    assert.equal(started.env.DEEPSEEK_API_KEY, undefined, 'no provider keys');
    assert.equal(started.env.HOME, join(root, 'home'));
    // microsandbox's TLS edge is trusted through these; without them Headroom can't reach Anthropic.
    assert.equal(started.env.SSL_CERT_FILE, '/etc/ssl/certs/ca-certificates.crt');
    assert.equal(started.env.REQUESTS_CA_BUNDLE, '/etc/ssl/certs/ca-certificates.crt');
    assert.equal(started.env.CURL_CA_BUNDLE, undefined, 'only what the runner has');
    assert.ok(logs.some((l) => l.level === 'info' && l.message.includes('upstream: http://127.0.0.1:9000')));

    await headroom.close();
    await assert.rejects(fetch(`${headroom.url}/health`, { signal: AbortSignal.timeout(2000) }), 'closed means stopped');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('a Headroom that exits or never becomes healthy is a warning, never a failed colony', async () => {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-headroom-'));
  try {
    const crashed = collect();
    const crashes = fakeBundle(join(root, 'a'), 'crashes');
    assert.equal(await startHeadroom({ env: { PATH: process.env.PATH, COLONIZER_HEADROOM_HOME: join(root, 'h') }, dir: crashes.dir, log: crashed.log, timeoutMs: 20_000 }), null);
    assert.equal(crashed.logs.at(-1).level, 'warn');
    assert.match(crashed.logs.at(-1).message, /exited: 3/);
    assert.match(crashed.logs.at(-1).message, /no module named headroom/, 'its output is in the warning');

    const hung = collect();
    const silent = fakeBundle(join(root, 'b'), 'silent');
    const t0 = Date.now();
    assert.equal(await startHeadroom({ env: { PATH: process.env.PATH, COLONIZER_HEADROOM_HOME: join(root, 'h') }, dir: silent.dir, log: hung.log, timeoutMs: 1500 }), null);
    assert.match(hung.logs.at(-1).message, /not healthy within 2 s/);
    assert.ok(Date.now() - t0 < 10_000, 'gives up after the timeout and stops the process');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
