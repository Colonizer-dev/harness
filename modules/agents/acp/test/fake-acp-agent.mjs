#!/usr/bin/env node
// A scriptable fake ACP agent for the runner's contract tests: it answers `initialize`, `session/new`
// and `session/load`, records everything it receives to ACP_FAKE_RECORD (one JSON object per line), and
// drives each `session/prompt` turn from the script named by ACP_FAKE_SCRIPT:
//
//   {
//     "handshake": { "protocolVersion": 1 },       // the initialize result (optional)
//     "models":    { "currentModelId": "m-1", ... },// merged into session/new when present
//     "setModel":  { "bad-model": true },           // model ids session/set_model refuses
//     "replay":    [session/update params],         // what session/load replays before its reply
//     "turns": {
//       "<exact prompt text>": step,               // keyed turns...
//       "*": step                                  // ...with this as the default
//     }
//   }
//
// A step is { "updates": [session/update params], "asks": [{method, params}], "stopReason":
// "end_turn", "die": 3, "dieAfter": 3 }: updates are emitted as notifications before the response,
// asks are agent→client requests whose runner replies are recorded ({asked, params, response}),
// "die" exits before the response and "dieAfter" shortly after it.

import { appendFileSync, existsSync, readFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

// Without a script there is nothing to serve — this also covers `node --test`, which discovers
// this file among the tests but does not drive it.
const script = process.env.ACP_FAKE_SCRIPT ? JSON.parse(readFileSync(process.env.ACP_FAKE_SCRIPT, 'utf8')) : null;
if (!script) process.exit(0);

const sessionId = process.env.ACP_FAKE_SESSION ?? 'sess-fake-1';
const note = (entry) => process.env.ACP_FAKE_RECORD && appendFileSync(process.env.ACP_FAKE_RECORD, `${JSON.stringify(entry)}\n`);
// ACP_FAKE_STATE is the fake's session store (the stand-in for the module's persisted dir): the ids
// `session/new` has handed out, one per line, so a later fake process can `session/load` them again.
const statePath = process.env.ACP_FAKE_STATE ?? null;
const remembered = () => (statePath && existsSync(statePath) ? readFileSync(statePath, 'utf8').split('\n').filter(Boolean) : []);
const remember = (id) => statePath && appendFileSync(statePath, `${id}\n`);

let nextId = 0;
const replies = new Map(); // our request id -> resolve
const reply = (id, result) => process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id, result })}\n`);
const replyError = (id, error) => process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id, error })}\n`);
const call = (method, params) =>
  new Promise((resolve) => {
    const id = ++nextId;
    replies.set(id, resolve);
    process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
  });
const notify = (method, params) => process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', method, params })}\n`);

async function runTurn(step, requestId) {
  for (const update of step.updates ?? []) notify('session/update', { sessionId, update });
  for (const ask of step.asks ?? []) {
    note({ asked: ask.method, params: ask.params, response: await call(ask.method, ask.params) });
  }
  if (step.die) process.exit(step.die === true ? 3 : step.die);
  reply(requestId, { stopReason: step.stopReason ?? 'end_turn' });
  if (step.dieAfter) setTimeout(() => process.exit(step.dieAfter === true ? 3 : step.dieAfter), 20).unref();
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on('line', async (line) => {
  if (!line.trim()) return;
  const message = JSON.parse(line);
  note({ method: message.method ?? null, params: message.params ?? null, result: message.result, error: message.error });
  const waiter = message.id !== undefined && replies.get(message.id);
  if (waiter) {
    replies.delete(message.id);
    waiter(message.error ? { error: message.error } : { result: message.result });
    return;
  }
  switch (message.method) {
    case 'initialize':
      if (script.handshake?.die) process.exit(script.handshake.die);
      reply(message.id, script.handshake ?? { protocolVersion: 1, agentCapabilities: {}, authMethods: [] });
      break;
    case 'session/new':
      remember(sessionId);
      reply(message.id, { sessionId, ...(script.models ? { models: script.models } : {}) });
      break;
    case 'session/load':
      // The ACP resume: replay the recorded conversation as updates, then answer — an id the store
      // does not know (or an agent that never advertised loadSession) is an error.
      if (!script.handshake?.agentCapabilities?.loadSession || !remembered().includes(message.params?.sessionId)) {
        replyError(message.id, { code: -32000, message: `no such session: ${message.params?.sessionId ?? ''}` });
        break;
      }
      for (const update of script.replay ?? []) notify('session/update', { sessionId: message.params.sessionId, update });
      reply(message.id, {});
      break;
    case 'session/prompt':
      await runTurn(script.turns?.[message.params.prompt?.[0]?.text] ?? script.turns?.['*'] ?? {}, message.id);
      break;
    case 'session/set_model':
      if (script.setModel?.[message.params?.modelId]) replyError(message.id, { code: -32000, message: `no such model: ${message.params.modelId}` });
      else reply(message.id, {});
      break;
    default:
      break; // e.g. session/cancel: a notification the fake acts on by doing nothing
  }
});
