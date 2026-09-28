#!/usr/bin/env node
// A stub `codex` CLI for the runner's contract tests. It answers `--version` and, for a headless
// turn (`-` prompt on stdin), emits scripted `--json` JSONL events, so the tests can drive every
// path without OpenAI, a key or a network. Behaviour is steered by env vars:
//
//   CODEX_FAKE_RECORD        append one JSON line {argv, prompt, env} per invocation here
//   CODEX_FAKE_VERSION       what `--version` prints (default 0.156.1, the pinned version)
//   CODEX_FAKE_SCRIPT        NDJSON file of --json events to emit instead of the defaults
//   CODEX_FAKE_THREAD_ID     overrides the thread.started thread_id (default thread-fake-1)
//   CODEX_FAKE_NO_COMPLETE   drop the turn.completed event (a child that ends without one)
//   CODEX_FAKE_TURN_FAILED   emit turn.failed after the item events
//   CODEX_FAKE_EXIT          exit code after emitting the events (default 0)
//   CODEX_FAKE_STDERR        text to write to stderr before exiting
//   CODEX_FAKE_SLEEP_MS      sleep before emitting, so a turn can be interrupted
//   CODEX_FAKE_SLEEP_FIRST   when set, only the first invocation sleeps (later turns recover)
//   CODEX_FAKE_IGNORE_SIGINT when set, SIGINT is ignored: only the runner's SIGKILL stops the fake
//   CODEX_FAKE_ASK           JSON questions for an ask_user call to the colonizer MCP server that
//                            $CODEX_HOME/config.toml registers; the round trip is recorded and the
//                            call streams as an mcp_tool_call item pair

import { appendFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { dirname, join } from 'node:path';

const argv = process.argv.slice(2);
const recordPath = process.env.CODEX_FAKE_RECORD;
// The invocation count of prompt turns only (the preflight's `--version` call records too, but
// must not consume a SLEEP_FIRST slot the interrupt tests rely on).
let invocation = 0;
if (recordPath) {
  mkdirSync(dirname(recordPath), { recursive: true });
  invocation = existsSync(recordPath)
    ? readFileSync(recordPath, 'utf8').trim().split('\n').filter(Boolean).filter((l) => !JSON.parse(l).argv.includes('--version')).length
    : 0;
}

// The env slice the tests assert on: the nesting decisions the runner must apply to every child.
const record = () => {
  if (!recordPath) return;
  appendFileSync(
    recordPath,
    `${JSON.stringify({
      argv,
      prompt: readFileSync(0, 'utf8'),
      env: {
        BROWSER: process.env.BROWSER ?? null,
        CODEX_HOME: process.env.CODEX_HOME ?? null,
        CODEX_API_KEY: process.env.CODEX_API_KEY ? 'set' : 'unset',
      },
    })}\n`,
  );
};

if (argv.includes('--version')) {
  record();
  process.stdout.write(`codex-cli ${process.env.CODEX_FAKE_VERSION ?? '0.156.1'}\n`);
  process.exit(0);
}

record();

// Reads the colonizer MCP server registration out of $CODEX_HOME/config.toml the way codex would
// and speaks newline-delimited JSON-RPC 2.0 to it, so the tests exercise mcp.mjs through a real
// child instead of posting at the bridge directly. Minimal TOML: flat `key = value` lines, where a
// JSON value parses as the JSON it is (the runner writes only basic strings, numbers and arrays).
const mcpServerFromConfig = (configPath) => {
  const tables = {};
  let table = '';
  for (const line of readFileSync(configPath, 'utf8').split('\n')) {
    const header = /^\[(.+)\]\s*$/.exec(line);
    if (header) { table = header[1]; continue; }
    const entry = /^([A-Za-z0-9_-]+) = (.+?)\s*$/.exec(line);
    if (entry && table) (tables[table] ??= {})[entry[1]] = JSON.parse(entry[2]);
  }
  const server = tables['mcp_servers.colonizer'] ?? {};
  if (!server.command) throw new Error('config.toml registers no mcp_servers.colonizer');
  return { command: server.command, args: server.args ?? [], env: tables['mcp_servers.colonizer.env'] ?? {} };
};

async function askViaConfig(configPath, questions) {
  const { command, args, env } = mcpServerFromConfig(configPath);
  const child = spawn(command, args, { env: { ...process.env, ...env }, stdio: ['pipe', 'pipe', 'inherit'] });
  let buf = '';
  const waiting = new Map();
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    for (let at; (at = buf.indexOf('\n')) >= 0;) {
      const line = buf.slice(0, at).trim();
      buf = buf.slice(at + 1);
      if (!line) continue;
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue; // progress notifications get no waiter
      waiting.get(msg.id)?.(msg);
      waiting.delete(msg.id);
    }
  });
  let nextId = 0;
  const rpc = (method, params) => new Promise((resolve) => { const id = ++nextId; waiting.set(id, resolve); child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`); });
  const tools = await rpc('tools/list');
  const call = await rpc('tools/call', { name: 'ask_user', arguments: { questions }, _meta: { progressToken: 7 } });
  child.kill('SIGKILL');
  return { tools: (tools.result?.tools ?? []).map((tool) => tool.name), result: JSON.parse(call.result?.content?.[0]?.text ?? 'null'), isError: Boolean(call.result?.isError) };
}

const threadId = process.env.CODEX_FAKE_THREAD_ID ?? 'thread-fake-1';
const defaultEvents = () => [
  { type: 'thread.started', thread_id: threadId },
  { type: 'turn.started' },
  { type: 'item.started', item: { id: 'item_1', type: 'command_execution', command: 'bash -lc ls', status: 'in_progress' } },
  { type: 'item.completed', item: { id: 'item_1', type: 'command_execution', command: 'bash -lc ls', aggregated_output: 'src\nREADME.md', exit_code: 0, status: 'completed' } },
  { type: 'item.completed', item: { id: 'item_2', type: 'reasoning', text: 'weighing the options' } },
  { type: 'item.completed', item: { id: 'item_3', type: 'agent_message', text: 'Hello, colony' } },
  { type: 'turn.completed', usage: { input_tokens: 10, cached_input_tokens: 2, output_tokens: 5 } },
];

// A script line that is not JSON passes through raw: a stream the runner must tolerate.
const events = (process.env.CODEX_FAKE_SCRIPT ? readFileSync(process.env.CODEX_FAKE_SCRIPT, 'utf8') : '')
  .trim()
  .split('\n')
  .filter(Boolean)
  .map((line) => {
    try {
      return JSON.parse(line);
    } catch {
      return line;
    }
  });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
// The interrupt test needs a child that outlives the runner's SIGINT: then an ask released before
// the turn_end can only have been cancelled at the interrupt, not at the child's exit.
if (process.env.CODEX_FAKE_IGNORE_SIGINT === '1') process.on('SIGINT', () => {});
if (process.env.CODEX_FAKE_SLEEP_MS && (!process.env.CODEX_FAKE_SLEEP_FIRST || invocation === 0)) {
  await sleep(Number(process.env.CODEX_FAKE_SLEEP_MS));
}
// The ask stands in for the model calling ask_user mid-turn: it parks on the bridge until the
// test's `answer` command resolves it, then rides the stream as the mcp_tool_call item pair codex
// emits for an MCP call.
let ask = null;
if (process.env.CODEX_FAKE_ASK) {
  ask = await askViaConfig(join(process.env.CODEX_HOME ?? '.', 'config.toml'), JSON.parse(process.env.CODEX_FAKE_ASK));
  if (recordPath) appendFileSync(recordPath, `${JSON.stringify({ argv, ask })}\n`);
}
const mcpItems = ask
  ? [
      { type: 'item.started', item: { id: 'item_mcp_1', type: 'mcp_tool_call', server: 'colonizer', tool: 'ask_user', status: 'in_progress' } },
      { type: 'item.completed', item: { id: 'item_mcp_1', type: 'mcp_tool_call', server: 'colonizer', tool: 'ask_user', status: 'completed', aggregated_output: JSON.stringify(ask.result ?? { error: 'no result' }) } },
    ]
  : [];
const stream = events.length ? events : defaultEvents();
stream.splice(2, 0, ...mcpItems); // after thread.started and turn.started, like a mid-turn call
for (const event of stream) {
  if (process.env.CODEX_FAKE_NO_COMPLETE === '1' && event.type === 'turn.completed') continue;
  process.stdout.write(`${typeof event === 'string' ? event : JSON.stringify(event)}\n`);
}
if (process.env.CODEX_FAKE_TURN_FAILED) {
  process.stdout.write(`${JSON.stringify({ type: 'turn.failed', error: { message: process.env.CODEX_FAKE_TURN_FAILED } })}\n`);
}
if (process.env.CODEX_FAKE_STDERR) process.stderr.write(process.env.CODEX_FAKE_STDERR);
process.exit(Number(process.env.CODEX_FAKE_EXIT ?? 0));
