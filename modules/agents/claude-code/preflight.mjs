/**
 * Pre-flight scan of the workspace, before the agent sees it.
 *
 * Runs *inside the colony*, never on the mothership. The repository being
 * scanned is attacker-controlled the moment a colony works on anything that is
 * not ours, and the mothership holds every credential; pointing a large parser
 * at hostile input belongs on the side of the boundary that is disposable.
 *
 * This is advisory, and the docs say so in as many words. A repository's own
 * `.claude/settings.json` hooks and `.mcp.json` servers already run inside
 * colonies by design. The boundary is the microVM, the publish sanitizer and a
 * human reading the pull request — not this. What a scan protects is the task
 * outcome: prompt injection steering the agent into work nobody asked for.
 *
 * The scanner itself is not bundled. `COLONIZER_SCAN_COMMAND` names one that
 * was mounted read-only into the colony, so the harness carries a mechanism
 * rather than a vendor.
 */

import { spawn } from 'node:child_process';

export const SCAN_MODES = new Set(['off', 'warn', 'block']);

/** Findings are noisy; a colony log is not the place for a megabyte of them. */
const MAX_OUTPUT = 20000;

export function scanMode(env = {}) {
  const raw = String(env.COLONIZER_SCAN ?? '').trim().toLowerCase();
  return SCAN_MODES.has(raw) ? raw : 'off';
}

/**
 * @returns {{argv: string[]|null, reason?: string}} the command to run, or why not.
 */
export function scanCommand(env = {}) {
  const mode = scanMode(env);
  if (mode === 'off') return { argv: null, reason: 'scanning is off' };
  const raw = String(env.COLONIZER_SCAN_COMMAND ?? '').trim();
  if (!raw) return { argv: null, reason: 'no scanner is configured' };
  // Split on whitespace only: no shell, so nothing in the setting can be
  // interpreted as a pipeline, a redirect or a second command.
  return { argv: raw.split(/\s+/).filter(Boolean) };
}

/**
 * Runs the configured scanner over the workspace.
 *
 * Resolves `{ ran, mode, code, findings }`. A scanner that cannot be started,
 * or that runs past its timeout, is reported and treated as no findings: a
 * broken scanner must not be able to stop every colony in `block` mode.
 *
 * @param {object} args
 * @param {object} args.env
 * @param {Function} args.emit  writes one protocol event
 * @param {string} [args.cwd]
 * @param {number} [args.timeoutMs]
 * @param {Function} [args.spawnFn]  injected for tests
 */
export async function runPreflight({ env = {}, emit, cwd = '/workspace', timeoutMs = 120000, spawnFn = spawn }) {
  const mode = scanMode(env);
  const { argv, reason } = scanCommand(env);
  if (!argv) {
    if (mode !== 'off') emit({ type: 'log', level: 'info', message: `pre-flight scan skipped: ${reason}` });
    return { ran: false, mode, code: null, findings: '' };
  }

  emit({ type: 'log', level: 'info', message: `pre-flight scan (${mode}): ${argv.join(' ')}` });

  const result = await new Promise((resolve) => {
    let out = '';
    let settled = false;
    const done = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };

    let child;
    try {
      child = spawnFn(argv[0], argv.slice(1), { cwd, stdio: ['ignore', 'pipe', 'pipe'] });
    } catch (e) {
      return done({ code: null, out: '', error: e?.message || String(e) });
    }

    const take = (chunk) => {
      if (out.length < MAX_OUTPUT) out += chunk.toString();
    };
    child.stdout?.on('data', take);
    child.stderr?.on('data', take);
    child.on('error', (e) => done({ code: null, out, error: e?.message || String(e) }));

    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      done({ code: null, out, error: `scanner did not finish within ${Math.round(timeoutMs / 1000)}s` });
    }, timeoutMs);
    if (timer.unref) timer.unref();

    child.on('close', (code) => {
      clearTimeout(timer);
      done({ code, out });
    });
  });

  const findings = result.out.slice(0, MAX_OUTPUT).trim();

  if (result.error) {
    // Deliberately not fatal, even in block mode. A scanner that fails to start
    // is an operator problem; making it able to halt every colony turns an
    // advisory feature into an outage.
    emit({ type: 'log', level: 'warn', message: `pre-flight scan did not run: ${result.error}` });
    return { ran: false, mode, code: null, findings };
  }

  if (findings) {
    emit({ type: 'log', level: result.code === 0 ? 'info' : 'warn', message: `pre-flight scan findings:\n${findings}` });
  } else {
    emit({ type: 'log', level: 'info', message: 'pre-flight scan found nothing' });
  }

  return { ran: true, mode, code: result.code, findings };
}

/** Whether a completed scan should stop the colony before the agent starts. */
export function shouldBlock(result) {
  return result.ran && result.mode === 'block' && result.code !== 0;
}
