#!/usr/bin/env node
// A stub `grok` CLI for the runner's contract tests. It answers `--version`, refuses `login`, and
// for a headless prompt (`-p`/`--prompt-file`) emits scripted streaming-json events, so the tests
// can drive every path without xAI, a key or a network. Behaviour is steered by env vars:
//
//   GROK_FAKE_RECORD        append one JSON line {argv, prompt, env} per invocation here
//   GROK_FAKE_VERSION       what `--version` prints (default 1.0.34, the pinned version)
//   GROK_FAKE_SCRIPT        NDJSON file of streaming-json events to emit instead of the defaults
//   GROK_FAKE_SESSION_ID    overrides the `end` event's sessionId
//   GROK_FAKE_NO_END        drop every `end` event (a child that exits without one)
//   GROK_FAKE_EXIT          exit code after emitting the events (default 0)
//   GROK_FAKE_STDERR        text to write to stderr before exiting
//   GROK_FAKE_SLEEP_MS      sleep before emitting, so a turn can be interrupted
//   GROK_FAKE_SLEEP_FIRST   when set, only the first invocation sleeps (later turns recover)
//   GROK_FAKE_ASK           JSON question list to POST to the runner's bridge (read from
//                           $GROK_HOME/config.toml) instead of the scripted events, like a
//                           colonizer__ask_user call; the bridge's answer becomes the turn text
//   GROK_FAKE_ASK_RECORD    append the bridge's JSON answer per ask here

import { appendFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

const argv = process.argv.slice(2);
const recordPath = process.env.GROK_FAKE_RECORD;
let invocation = 0;
if (recordPath) {
  mkdirSync(dirname(recordPath), { recursive: true });
  invocation = existsSync(recordPath) ? readFileSync(recordPath, 'utf8').trim().split('\n').filter(Boolean).length : 0;
}

// The env slice the tests assert on: the nesting decisions the runner must apply to every child.
const record = () => {
  if (!recordPath) return;
  const promptFile = argv.includes('--prompt-file') ? argv[argv.indexOf('--prompt-file') + 1] : null;
  appendFileSync(
    recordPath,
    `${JSON.stringify({
      argv,
      prompt: promptFile ? readFileSync(promptFile, 'utf8') : argv.includes('-p') ? (argv[argv.indexOf('-p') + 1] ?? null) : null,
      env: {
        BROWSER: process.env.BROWSER ?? null,
        GROK_HOME: process.env.GROK_HOME ?? null,
        GROK_MEMORY: process.env.GROK_MEMORY ?? null,
        GROK_TELEMETRY_ENABLED: process.env.GROK_TELEMETRY_ENABLED ?? null,
        GROK_DISABLE_AUTOUPDATER: process.env.GROK_DISABLE_AUTOUPDATER ?? null,
        XAI_API_KEY: process.env.XAI_API_KEY ? 'set' : 'unset',
      },
    })}\n`,
  );
};

if (argv.includes('--version')) {
  record();
  process.stdout.write(`grok ${process.env.GROK_FAKE_VERSION ?? '1.0.34'}\n`);
  process.exit(0);
}
if (argv[0] === 'login') {
  record(); // the runner must never get here; the tests assert the record stays free of it
  process.stderr.write('fake grok refuses login\n');
  process.exit(3);
}

record();

// Ask mode (the Questions contract tests): read the runner's bridge registration out of
// $GROK_HOME/config.toml and drive colonizer__ask_user exactly like grok's MCP client would — a
// tool_call/tool_call_update pair around a POST /ask — then end the turn on the bridge's answer.
if (process.env.GROK_FAKE_ASK) {
  let config = '';
  try {
    config = readFileSync(join(process.env.GROK_HOME ?? '.', 'config.toml'), 'utf8');
  } catch {
    // reported below, as a failed tool call
  }
  const bridgeUrl = /COLONIZER_BRIDGE_URL\s*=\s*"([^"]+)"/.exec(config)?.[1];
  const bridgeToken = /COLONIZER_BRIDGE_TOKEN\s*=\s*"([^"]+)"/.exec(config)?.[1];
  const questions = JSON.parse(process.env.GROK_FAKE_ASK);
  const out = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
  out({ type: 'tool_call', toolCallId: 'call_ask', title: 'colonizer ask_user', kind: 'mcp', status: 'in_progress', toolName: 'colonizer__ask_user', rawInput: { questions } });
  let answer;
  let status = 'completed';
  if (!bridgeUrl || !bridgeToken) {
    answer = { error: 'no colonizer bridge registered in GROK_HOME/config.toml' };
    status = 'failed';
  } else {
    try {
      const res = await fetch(`${bridgeUrl}/ask`, { method: 'POST', headers: { authorization: `Bearer ${bridgeToken}`, 'content-type': 'application/json' }, body: JSON.stringify({ questions }) });
      answer = await res.json();
    } catch (error) {
      answer = { error: String(error?.message ?? error) };
      status = 'failed';
    }
  }
  out({ type: 'tool_call_update', toolCallId: 'call_ask', status, rawOutput: answer });
  out({ type: 'text', data: `answered: ${JSON.stringify(answer)}` });
  if (process.env.GROK_FAKE_ASK_RECORD) appendFileSync(process.env.GROK_FAKE_ASK_RECORD, `${JSON.stringify(answer)}\n`);
  out({
    type: 'end',
    stopReason: 'end_turn',
    sessionId: process.env.GROK_FAKE_SESSION_ID ?? 'sess-fake-1',
    total_cost_usd: 0.01,
    usage: { input_tokens: 10, output_tokens: 5 },
    modelUsage: { 'grok-4.5': { inputTokens: 10, outputTokens: 5, cacheReadInputTokens: 2, modelCalls: 1, costUSD: 0.01 } },
  });
  process.exit(0);
}

const defaultEvents = () => [
  { type: 'thought', data: 'weighing the options' },
  { type: 'tool_call', toolCallId: 'call_1', title: 'Read', kind: 'read', status: 'in_progress', toolName: 'read_file', rawInput: { path: 'src/main.rs' } },
  { type: 'tool_call_update', toolCallId: 'call_1', status: 'in_progress', content: [] },
  { type: 'tool_call_update', toolCallId: 'call_1', status: 'completed', rawOutput: { lines: 42 } },
  { type: 'text', data: 'Hello' },
  { type: 'text', data: ', colony' },
  {
    type: 'end',
    stopReason: 'end_turn',
    sessionId: process.env.GROK_FAKE_SESSION_ID ?? 'sess-fake-1',
    requestId: 'req_1',
    total_cost_usd: 0.01,
    usage: { input_tokens: 10, output_tokens: 5 },
    modelUsage: { 'grok-4.5': { inputTokens: 10, outputTokens: 5, cacheReadInputTokens: 2, modelCalls: 1, costUSD: 0.01 } },
  },
];

const events = (process.env.GROK_FAKE_SCRIPT ? readFileSync(process.env.GROK_FAKE_SCRIPT, 'utf8') : '')
  .trim()
  .split('\n')
  .filter(Boolean)
  .map((line) => JSON.parse(line));

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
if (process.env.GROK_FAKE_SLEEP_MS && (!process.env.GROK_FAKE_SLEEP_FIRST || invocation === 0)) {
  await sleep(Number(process.env.GROK_FAKE_SLEEP_MS));
}
for (const event of events.length ? events : defaultEvents()) {
  if (process.env.GROK_FAKE_NO_END === '1' && event.type === 'end') continue;
  process.stdout.write(`${JSON.stringify(event)}\n`);
}

if (process.env.GROK_FAKE_STDERR) process.stderr.write(process.env.GROK_FAKE_STDERR);
process.exit(Number(process.env.GROK_FAKE_EXIT ?? 0));
