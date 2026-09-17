// Headroom inside a colony (docs/protocol.md, "Token savings"). When the mothership has mounted the Headroom
// bundle, the runner starts `headroom proxy` on loopback between Claude Code and whatever Claude Code would
// otherwise talk to: the model router when the colony has routes, Anthropic when it doesn't. Routing,
// fallback and the TLS edge that adds the real credential for api.anthropic.com all stay where they are.
//
// The flags and environment are the ones scripts/headroom-bundle/smoke.py checks before a bundle is
// published: no network of its own, no response cache, no filesystem writes, no ML model.

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { createServer } from 'node:net';
import { join } from 'node:path';

export const HEADROOM_DIR = '/opt/colonizer/headroom';

export const HEADROOM_ENV = Object.freeze({
  // No egress from Headroom itself: telemetry, update checks, license reporting and model downloads.
  HEADROOM_OFFLINE: '1',
  HEADROOM_BEACON: 'off',
  DO_NOT_TRACK: '1',
  // It would poll Anthropic's usage API with the colony's placeholder credential.
  HEADROOM_NO_SUBSCRIPTION_TRACKING: '1',
  // Kompress needs a 261 MB model the bundle doesn't carry, and never runs in the default cache mode.
  HEADROOM_DISABLE_KOMPRESS: '1',
  // LiteLLM fetches its price map from GitHub at import, outside Headroom's offline switch.
  LITELLM_LOCAL_MODEL_COST_MAP: 'True',
  HF_HUB_OFFLINE: '1',
  TRANSFORMERS_OFFLINE: '1',
  // The bundle is mounted read-only.
  PYTHONDONTWRITEBYTECODE: '1',
});

/**
 * `headroom proxy` arguments. `--no-cache` because its semantic cache answers a similar-enough request
 * without reaching the model, which an agent must never get; `--stateless` because nothing it would
 * write is worth keeping in a disposable colony.
 */
export function headroomArgs(port, upstream) {
  const args = ['-m', 'headroom.cli', 'proxy', '--host', '127.0.0.1', '--port', String(port), '--stateless', '--no-cache'];
  if (upstream) args.push('--anthropic-api-url', upstream);
  return args;
}

/**
 * Passed through from the runner: where the colony's trusted certificates are. microsandbox sets these to a
 * bundle that includes the CA of its TLS edge, which Headroom's Python has to trust to reach Anthropic;
 * without them a request with no model router in front fails TLS and Claude Code sees a 502.
 */
const PASSED_ENV = ['PATH', 'LANG', 'SSL_CERT_FILE', 'SSL_CERT_DIR', 'REQUESTS_CA_BUNDLE', 'CURL_CA_BUNDLE'];

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.unref();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

/**
 * Starts Headroom and resolves once it is healthy, to `{ url, close }`, or to null when it can't run:
 * saving tokens is never a reason for a colony not to start, so every failure is a warning.
 */
export async function startHeadroom({
  env = process.env,
  upstream,
  log = () => {},
  dir = HEADROOM_DIR,
  timeoutMs = 90_000,
  spawnImpl = spawn,
  fetchImpl = fetch,
} = {}) {
  const python = join(dir, 'python', 'bin', 'python3');
  if (!existsSync(python)) {
    log({ level: 'warn', message: `Headroom is switched on but its bundle is not mounted at ${dir}; running without it` });
    return null;
  }
  const home = env.COLONIZER_HEADROOM_HOME || '/var/lib/colonizer/headroom';
  try {
    mkdirSync(home, { recursive: true });
  } catch {
    // Headroom runs stateless; a home it can't create only matters to libraries that want a cache dir.
  }
  const port = await freePort();
  // Nothing else from the runner's environment: Headroom needs no credential of its own, since the
  // requests it forwards carry theirs.
  const passed = Object.fromEntries(PASSED_ENV.filter((key) => env[key]).map((key) => [key, env[key]]));
  const child = spawnImpl(python, headroomArgs(port, upstream), {
    env: { PATH: '/usr/local/bin:/usr/bin:/bin', LANG: 'C.UTF-8', ...passed, HOME: home, ...HEADROOM_ENV },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  // The last lines of its output, for the warning when it doesn't come up.
  const tail = [];
  const keep = (chunk) => {
    for (const line of String(chunk).split('\n')) if (line.trim()) tail.push(line);
    tail.splice(0, Math.max(0, tail.length - 12));
  };
  child.stdout?.on('data', keep);
  child.stderr?.on('data', keep);
  let exited = null;
  child.on('exit', (code, signal) => {
    exited = signal ?? code;
  });
  child.on('error', (error) => {
    exited = error.message;
  });

  const url = `http://127.0.0.1:${port}`;
  const close = async () => {
    if (exited !== null) return;
    child.kill('SIGTERM');
    await new Promise((resolve) => {
      const force = setTimeout(() => {
        child.kill('SIGKILL');
        resolve();
      }, 5000);
      child.once('exit', () => {
        clearTimeout(force);
        resolve();
      });
    });
  };

  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    if (exited !== null) break;
    try {
      const res = await fetchImpl(`${url}/health`, { signal: AbortSignal.timeout(2000) });
      if (res.ok) {
        log({ level: 'info', message: `Headroom compressing on ${url} (upstream: ${upstream ?? 'Anthropic'}), ready in ${Math.round((Date.now() - started) / 100) / 10} s` });
        return { url, close };
      }
    } catch {
      // not listening yet
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  // Decided before close(), whose SIGTERM would otherwise read as the reason.
  const why = exited !== null ? `it exited: ${exited}` : `not healthy within ${Math.round(timeoutMs / 1000)} s`;
  await close();
  log({ level: 'warn', message: `Headroom did not start (${why}); running without it.${tail.length ? ` Last output: ${tail.slice(-3).join(' | ')}` : ''}` });
  return null;
}
