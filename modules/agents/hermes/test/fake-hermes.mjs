#!/usr/bin/env node
// A stub of the Hermes Agent CLI for runner.mjs's tests: parses the same argv the runner builds,
// records argv, the parsed HERMES_HOME config and the child env to $FAKE_HERMES_RECORD, then emits
// the stream-json shapes verified against hermes-agent v2026.9.24. FAKE_HERMES_MODE picks a variant:
// slow (deltas with gaps), hang (init then nothing), failure (result with error, exit 1), noise (a
// non-JSON line first, as the tirith scanner prints), ask (calls the colonizer ask_user tool the
// way Hermes would hand an MCP tool call to mcp.mjs, by POSTing the recorded bridge env's /ask).
// --version answers like the real binary. FAKE_HERMES_IGNORE_SIGTERM makes the fake ignore SIGTERM,
// so only the runner's SIGKILL stops it; FAKE_HERMES_HANG_MS overrides the hang length (default
// 120 s), so the interrupt test's regression path fails instead of hanging.

import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const VERSION = 'Hermes Agent v0.21.5 (2026.9.24)';

if (process.argv.includes('--version')) {
  process.stdout.write(`${VERSION}\n`);
  process.exit(0);
}

let config = {};
try {
  config = JSON.parse(readFileSync(join(process.env.HERMES_HOME ?? '', 'config.yaml'), 'utf8'));
} catch {
  // a missing config is itself something the recording should show
}
try {
  writeFileSync(
    process.env.FAKE_HERMES_RECORD ?? '/dev/null',
    JSON.stringify({ argv: process.argv.slice(2), config, env: { TERMINAL_ENV: process.env.TERMINAL_ENV ?? null, HERMES_HOME: process.env.HERMES_HOME ?? null } }, null, 2),
  );
} catch {
  // recording is best effort
}

const arg = (flag) => {
  const at = process.argv.indexOf(flag);
  return at >= 0 ? process.argv[at + 1] : undefined;
};
const text = arg('-q');
const session = arg('--resume') ?? 'fake-session-1';
const emit = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const mode = process.env.FAKE_HERMES_MODE ?? '';
// The interrupt test needs a child that outlives the runner's SIGTERM: then an ask released before
// the turn_end can only have been cancelled at the interrupt, not at the child's exit.
if (process.env.FAKE_HERMES_IGNORE_SIGTERM) process.on('SIGTERM', () => {});

if (text === undefined) {
  process.stderr.write('fake-hermes: expected a -q prompt\n');
  process.exit(2);
}

emit({ type: 'system', subtype: 'init', model: arg('-m') ?? 'unknown', session_id: session, timestamp: Date.now() });
if (mode === 'noise') process.stdout.write('tirith: security scanner found no issues in 0.4s\n');
if (mode === 'ask') {
  // The model called mcp__colonizer__ask_user: POST the question to the runner's loopback bridge
  // using the env block the config gave the colonizer MCP server, and surface what comes back.
  const bridgeEnv = config?.mcp_servers?.colonizer?.env ?? {};
  const question = { question: 'Ship it?', options: [{ label: 'Yes', description: 'open the pull request' }, { label: 'No' }] };
  emit({ type: 'tool_use', name: 'mcp__colonizer__ask_user', input: { questions: [question] }, timestamp: Date.now() });
  let answer;
  try {
    const res = await fetch(`${bridgeEnv.COLONIZER_BRIDGE_URL}/ask`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', authorization: `Bearer ${bridgeEnv.COLONIZER_BRIDGE_TOKEN}` },
      body: JSON.stringify({ questions: [question] }),
    });
    answer = await res.json();
  } catch (error) {
    answer = { bridge_error: String(error?.message ?? error) };
  }
  emit({ type: 'tool_result', name: 'mcp__colonizer__ask_user', output: JSON.stringify(answer), duration_ms: 5, is_error: false, timestamp: Date.now() });
  emit({ type: 'result', session_id: session, exit_code: 0, text: `Answered: ${JSON.stringify(answer)}`, tokens: { input: 100, output: 20, total: 120, cache_read: 40, cache_write: 10 }, duration_ms: 10, timestamp: Date.now() });
  process.exit(0);
}
if (mode !== 'hang') {
  emit({ type: 'text', text: 'Working ', timestamp: Date.now() });
  if (mode === 'slow') await sleep(4000);
  emit({ type: 'text', text: 'on it.', timestamp: Date.now() });
  emit({ type: 'tool_use', name: 'terminal', input: { command: 'ls' }, timestamp: Date.now() });
  emit({ type: 'tool_result', name: 'terminal', output: '{"stdout":"file.txt"}', duration_ms: 12, is_error: false, timestamp: Date.now() });
  if (mode === 'failure') {
    emit({ type: 'result', session_id: session, exit_code: 1, text: '', error: 'provider unreachable', tokens: { input: 1, output: 2, total: 3, cache_read: 0, cache_write: 0 }, duration_ms: 50, timestamp: Date.now() });
    process.exit(1);
  }
  emit({
    type: 'result',
    session_id: session,
    exit_code: 0,
    text: 'Working on it.',
    tokens: { input: 100, output: 20, total: 120, cache_read: 40, cache_write: 10 },
    duration_ms: 500,
    timestamp: Date.now(),
  });
}
if (mode === 'hang') await sleep(Number(process.env.FAKE_HERMES_HANG_MS) || 120_000);
