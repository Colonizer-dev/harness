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
//
// Cross-process resume is modelled too: a fresh thread writes a rollout file under
// $CODEX_HOME/sessions, and `resume <id>` fails (non-zero, on stderr, no thread.started) when that
// file is not there — the real CLI cannot resume a thread it holds no rollout for.

import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
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

const threadId = process.env.CODEX_FAKE_THREAD_ID ?? 'thread-fake-1';
// The rollout store, exactly where the real CLI keeps it: a fresh thread creates its file, `resume`
// answers with the named thread only if the file survived in the CODEX_HOME it was handed.
const home = process.env.CODEX_HOME;
const rollout = (id) => join(home, 'sessions', `${id}.jsonl`);
const resumeId = argv.includes('resume') ? argv[argv.indexOf('resume') + 1] : null;
if (home && !resumeId) {
  mkdirSync(join(home, 'sessions'), { recursive: true });
  writeFileSync(rollout(threadId), 'thread\n');
}
if (home && resumeId && !existsSync(rollout(resumeId))) {
  process.stderr.write(`no session rollout for thread ${resumeId} under ${home}\n`);
  process.exit(1);
}
const activeThread = resumeId ?? threadId; // a resumed turn reports the thread it resumed
const defaultEvents = () => [
  { type: 'thread.started', thread_id: activeThread },
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
if (process.env.CODEX_FAKE_SLEEP_MS && (!process.env.CODEX_FAKE_SLEEP_FIRST || invocation === 0)) {
  await sleep(Number(process.env.CODEX_FAKE_SLEEP_MS));
}
for (const event of events.length ? events : defaultEvents()) {
  if (process.env.CODEX_FAKE_NO_COMPLETE === '1' && event.type === 'turn.completed') continue;
  process.stdout.write(`${typeof event === 'string' ? event : JSON.stringify(event)}\n`);
}
if (process.env.CODEX_FAKE_TURN_FAILED) {
  process.stdout.write(`${JSON.stringify({ type: 'turn.failed', error: { message: process.env.CODEX_FAKE_TURN_FAILED } })}\n`);
}
if (process.env.CODEX_FAKE_STDERR) process.stderr.write(process.env.CODEX_FAKE_STDERR);
process.exit(Number(process.env.CODEX_FAKE_EXIT ?? 0));
