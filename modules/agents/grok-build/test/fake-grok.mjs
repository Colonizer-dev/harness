#!/usr/bin/env node
// A stub `grok` CLI for the runner's contract tests. It answers `--version`, refuses `login`, and
// for a headless prompt (`-p`/`--prompt-file`) emits scripted streaming-json events, so the tests
// can drive every path without xAI, a key or a network. Behaviour is steered by env vars:
//
//   GROK_FAKE_RECORD        append one JSON line {argv, prompt, env, tty, trustedFolders} per
//                           invocation here
//   GROK_FAKE_VERSION       what `--version` prints (default 1.0.34, the pinned version)
//   GROK_FAKE_SCRIPT        NDJSON file of streaming-json events to emit instead of the defaults
//   GROK_FAKE_SESSION_ID    overrides the `end` event's sessionId
//   GROK_FAKE_NO_END        drop every `end` event (a child that exits without one)
//   GROK_FAKE_EXIT          exit code after emitting the events (default 0)
//   GROK_FAKE_STDERR        text to write to stderr before exiting
//   GROK_FAKE_SLEEP_MS      sleep before emitting, so a turn can be interrupted
//   GROK_FAKE_SLEEP_FIRST   when set, only the first invocation sleeps (later turns recover)
//   GROK_FAKE_MCP_CALLS     JSON array of {name, arguments}: play the model and drive the colonizer
//                           MCP server the runner registered in $GROK_HOME/config.toml (initialize,
//                           tools/list, one tools/call per entry); the tool names and each call's
//                           outcome are recorded under `mcp`

import { spawn } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';

const argv = process.argv.slice(2);
const recordPath = process.env.GROK_FAKE_RECORD;
let invocation = 0;
if (recordPath) {
  mkdirSync(dirname(recordPath), { recursive: true });
  invocation = existsSync(recordPath) ? readFileSync(recordPath, 'utf8').trim().split('\n').filter(Boolean).length : 0;
}

// The env slice the tests assert on: the nesting decisions the runner must apply to every child.
const record = (extra = {}) => {
  if (!recordPath) return;
  const promptFile = argv.includes('--prompt-file') ? argv[argv.indexOf('--prompt-file') + 1] : null;
  appendFileSync(
    recordPath,
    `${JSON.stringify({
      argv,
      prompt: promptFile ? readFileSync(promptFile, 'utf8') : argv.includes('-p') ? (argv[argv.indexOf('-p') + 1] ?? null) : null,
      env: {
        BROWSER: process.env.BROWSER ?? null,
        GROK_FOLDER_TRUST: process.env.GROK_FOLDER_TRUST ?? null,
        GROK_HOME: process.env.GROK_HOME ?? null,
        GROK_MEMORY: process.env.GROK_MEMORY ?? null,
        GROK_TELEMETRY_ENABLED: process.env.GROK_TELEMETRY_ENABLED ?? null,
        GROK_DISABLE_AUTOUPDATER: process.env.GROK_DISABLE_AUTOUPDATER ?? null,
        GROK_MODELS_BASE_URL: process.env.GROK_MODELS_BASE_URL ?? null,
        GROK_CODE_XAI_API_KEY: process.env.GROK_CODE_XAI_API_KEY ? 'set' : 'unset',
        XAI_API_KEY: process.env.XAI_API_KEY ? 'set' : 'unset',
      },
      // The folder-trust preconditions the runner must hold: a headless child (no TTY on the pipes
      // grok resolves trust from) and a GROK_HOME whose trust store has no recorded grant.
      tty: { stdin: process.stdin.isTTY === true, stderr: process.stderr.isTTY === true },
      trustedFolders: process.env.GROK_HOME ? existsSync(join(process.env.GROK_HOME, 'trusted_folders.toml')) : false,
      ...extra,
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

/** The colonizer MCP server entry of the config.toml the runner wrote into GROK_HOME, parsed for just
 * the subset this runner writes (its values are JSON, which is valid TOML for strings and arrays). */
function mcpConfig(home) {
  let text = '';
  try {
    text = readFileSync(join(home ?? '', 'config.toml'), 'utf8');
  } catch {
    return {};
  }
  const cfg = {};
  let inTable = false;
  for (const line of text.split('\n')) {
    if (line.startsWith('[')) inTable = line.trim() === '[mcp_servers.colonizer]';
    else if (inTable && line.includes('=')) {
      const at = line.indexOf('=');
      const key = line.slice(0, at).trim();
      const raw = line.slice(at + 1).trim();
      try {
        cfg[key] = JSON.parse(raw);
      } catch {
        // The one value JSON.parse cannot read is the inline table: pull its `KEY = "json"` pairs.
        cfg[key] = Object.fromEntries([...raw.matchAll(/([A-Za-z0-9_-]+) = ("(?:[^"\\]|\\.)*")/g)].map(([_, k, v]) => [k, JSON.parse(v)]));
      }
    }
  }
  return cfg;
}

/** Play the model against the registered MCP server: initialize, tools/list, one tools/call per
 * scripted entry. Resolves with the tool names and each call's outcome, for the record. */
async function callMcp(cfg, calls) {
  const child = spawn(cfg.command, cfg.args ?? [], { env: { ...process.env, ...cfg.env }, stdio: ['pipe', 'pipe', 'inherit'] });
  const pending = new Map();
  let next = 0;
  createInterface({ input: child.stdout, crlfDelay: Infinity }).on('line', (line) => {
    const msg = JSON.parse(line);
    const waiter = pending.get(msg.id);
    if (waiter) {
      pending.delete(msg.id);
      waiter(msg);
    }
  });
  const rpc = (method, params) =>
    new Promise((resolve) => {
      const id = next++;
      pending.set(id, resolve);
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    });
  await rpc('initialize', {});
  const out = { tools: (await rpc('tools/list', {})).result?.tools?.map((t) => t.name) ?? [], calls: [] };
  for (const { name, arguments: input } of calls) {
    const reply = await rpc('tools/call', { name, arguments: input });
    out.calls.push({ name, isError: reply.result?.isError === true, text: reply.result?.content?.[0]?.text ?? null, error: reply.error?.message ?? null });
  }
  child.kill('SIGKILL');
  return out;
}

const scriptedCalls = process.env.GROK_FAKE_MCP_CALLS ? JSON.parse(process.env.GROK_FAKE_MCP_CALLS) : [];
const cfg = mcpConfig(process.env.GROK_HOME);
const mcp = process.env.GROK_FAKE_MCP_CALLS && cfg.command ? await callMcp(cfg, scriptedCalls) : undefined;
record(mcp ? { mcp } : {});

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
// The scripted MCP calls appear as grok tool_call/tool_call_update events (Grok names the tools
// `colonizer__<tool>` to the model), before the model's own events.
const mcpEvents = (mcp ?? { calls: [] }).calls.flatMap((call, i) => [
  { type: 'tool_call', toolCallId: `call_mcp_${i + 1}`, title: 'MCP', kind: 'mcp', status: 'in_progress', toolName: `colonizer__${call.name}`, rawInput: scriptedCalls[i]?.arguments ?? {} },
  { type: 'tool_call_update', toolCallId: `call_mcp_${i + 1}`, status: call.isError ? 'failed' : 'completed', rawOutput: call.text ?? call.error ?? '' },
]);
for (const event of [...mcpEvents, ...(events.length ? events : defaultEvents())]) {
  if (process.env.GROK_FAKE_NO_END === '1' && event.type === 'end') continue;
  process.stdout.write(`${JSON.stringify(event)}\n`);
}

if (process.env.GROK_FAKE_STDERR) process.stderr.write(process.env.GROK_FAKE_STDERR);
process.exit(Number(process.env.GROK_FAKE_EXIT ?? 0));
