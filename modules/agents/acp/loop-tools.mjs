#!/usr/bin/env node
// The loop tools for the runners without a colonizer MCP server of their own (issue #643):
// loop_stop for every loop colony, and loop_next when the loop is self-paced (docs/loops.md). The
// names, descriptions, schemas, the 15 min to 24 h clamp, the answers and the bridge wire
// (`POST /loop_next`, `POST /loop_stop`, bearer token) are the ones the codex, grok-build and hermes
// modules' mcp.mjs speak, so a loop paces itself the same way on every module.
//
// One file, three uses: the runner starts `createLoopBridge`, a loopback HTTP server that turns the
// calls into `loop_next` / `loop_stop` protocol events; the Pi module's loop extension imports
// `loopTools` and `callLoopTool`; and the ACP runner registers this file as a dependency-free stdio
// MCP server (newline-delimited JSON-RPC 2.0, protocolVersion 2024-11-05) on its session. acp holds
// the original and pi a copy that a test keeps byte-identical.
//
// The switches are the mothership's: COLONIZER_LOOP=true for a loop colony and
// COLONIZER_LOOP_SELF_PACED=true when it names its own next run (boot.rs). The bridge coordinates
// are COLONIZER_BRIDGE_URL and COLONIZER_BRIDGE_TOKEN.

import { randomBytes } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

// A self-paced loop's pacing bounds, the mothership's own (docs/loops.md): loop_next clamps into
// them here, so the number in its answer is the schedule the mothership records.
export const NEXT_MIN_MINUTES = 15;
export const NEXT_MAX_MINUTES = 24 * 60;

/** The mothership's loop switches, read from a runner or server env. */
export function loopSwitches(env = process.env) {
  const loop = env.COLONIZER_LOOP === 'true';
  return { loop, selfPaced: loop && env.COLONIZER_LOOP_SELF_PACED === 'true' };
}

/** The loop tools a colony is offered, as JSON Schema (MCP's inputSchema, a Pi tool's parameters). */
export function loopTools({ loop, selfPaced }) {
  return [
    loop && selfPaced && {
      name: 'loop_next',
      description: `Schedule this loop's next run: minutes from now (${NEXT_MIN_MINUTES} to ${NEXT_MAX_MINUTES}) and why.`,
      inputSchema: {
        type: 'object',
        properties: {
          delay_minutes: { type: 'integer', description: `Minutes from now; clamped to ${NEXT_MIN_MINUTES}–${NEXT_MAX_MINUTES}` },
          reason: { type: 'string', description: 'Why then: what the next run should find or do' },
        },
        required: ['delay_minutes', 'reason'],
      },
    },
    loop && {
      name: 'loop_stop',
      description: "End this loop: it will not run again until the operator re-enables it. Use when the loop's goal is met.",
      inputSchema: { type: 'object', properties: { reason: { type: 'string', description: 'Why the loop should stop' } }, required: ['reason'] },
    },
  ].filter(Boolean);
}

const refusal = (what, why) => ({ text: `Could not ${what}: ${why}`, isError: false });

async function forward({ url, token, fetchImpl }, path, body) {
  let res;
  try {
    res = await fetchImpl(`${url}${path}`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` }, body: JSON.stringify(body) });
  } catch (error) {
    return { error: `the colonizer bridge is unreachable: ${error?.message ?? error}` };
  }
  if (!res.ok) return { error: `the colonizer bridge answered ${res.status}` };
  try {
    return await res.json();
  } catch {
    return {};
  }
}

/** One loop tool call, forwarded over the bridge: `{text, isError}`, the answer the agent reads. */
export async function callLoopTool(name, args, { url, token, fetchImpl = fetch }) {
  const input = args && typeof args === 'object' ? args : {};
  const bridge = { url, token, fetchImpl };
  if (name === 'loop_next') {
    const delay = Number(input.delay_minutes);
    if (!Number.isFinite(delay) || delay < 1) return refusal('schedule the next run', 'delay_minutes must be a number of minutes from now.');
    if (!String(input.reason ?? '').trim()) return refusal('schedule the next run', 'reason is required — what the next run should find or do.');
    const minutes = Math.min(NEXT_MAX_MINUTES, Math.max(NEXT_MIN_MINUTES, Math.round(delay)));
    const data = await forward(bridge, '/loop_next', { delay_minutes: minutes, reason: String(input.reason) });
    if (data?.error) return { text: String(data.error), isError: true };
    return { text: `Next run scheduled in ${minutes} minutes.`, isError: false };
  }
  if (name === 'loop_stop') {
    if (!String(input.reason ?? '').trim()) return refusal('stop the loop', 'reason is required — why it should not run again.');
    const data = await forward(bridge, '/loop_stop', { reason: String(input.reason) });
    if (data?.error) return { text: String(data.error), isError: true };
    return { text: 'The loop is stopped; this is its last run.', isError: false };
  }
  throw new Error(`unknown loop tool ${name}`);
}

/** The runner's loopback bridge: `/loop_next` and `/loop_stop` leave the colony as protocol events
 * (docs/protocol.md §2), validated as the codex module's bridge validates them. The delay was
 * clamped by the caller; the bridge only refuses a shape it must not emit. */
export async function createLoopBridge({ emit, token = randomBytes(16).toString('hex') }) {
  const server = createServer((req, res) => {
    const reply = (status, payload) => {
      if (res.destroyed) return;
      res.writeHead(status, { 'content-type': 'application/json' });
      res.end(JSON.stringify(payload));
    };
    if (req.method !== 'POST' || req.headers.authorization !== `Bearer ${token}`) {
      reply(req.method !== 'POST' ? 404 : 401, {});
      return;
    }
    let body = '';
    req.on('data', (c) => {
      body += c;
      if (body.length > 1 << 20) req.destroy();
    });
    req.on('end', () => {
      let msg = null;
      try {
        msg = JSON.parse(body || '{}');
      } catch {
        reply(400, {});
        return;
      }
      if (req.url === '/loop_next') {
        const minutes = Number(msg.delay_minutes);
        if (!Number.isFinite(minutes) || minutes < 1) reply(200, { error: 'loop_next needs delay_minutes: a number of minutes from now' });
        else if (typeof msg.reason !== 'string' || !msg.reason.trim()) reply(200, { error: 'loop_next needs a reason: what the next run should find or do' });
        else {
          emit({ type: 'loop_next', delay_minutes: Math.round(minutes), reason: msg.reason });
          reply(200, { ok: true });
        }
      } else if (req.url === '/loop_stop') {
        if (typeof msg.reason !== 'string' || !msg.reason.trim()) reply(200, { error: 'loop_stop needs a reason: why the loop should stop' });
        else {
          emit({ type: 'loop_stop', reason: msg.reason });
          reply(200, { ok: true });
        }
      } else reply(404, {});
    });
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    token,
    close: () =>
      new Promise((resolve) => {
        server.close(resolve);
        server.closeAllConnections?.();
      }),
  };
}

// --- stdio MCP server --------------------------------------------------------------------------

function serve(env) {
  const tools = loopTools(loopSwitches(env));
  const names = tools.map((tool) => tool.name);
  const bridge = { url: env.COLONIZER_BRIDGE_URL ?? '', token: env.COLONIZER_BRIDGE_TOKEN ?? '' };
  const send = (msg) => process.stdout.write(`${JSON.stringify(msg)}\n`);
  const onMessage = async (msg) => {
    if (msg?.jsonrpc !== '2.0' || msg.id === undefined) return; // notifications get no reply
    try {
      let result;
      if (msg.method === 'initialize') result = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'colonizer_loop', version: '0.1.0' } };
      else if (msg.method === 'tools/list') result = { tools };
      else if (msg.method === 'tools/call') {
        const name = msg.params?.name;
        if (!names.includes(name)) throw Object.assign(new Error(`unknown tool ${name}`), { code: -32602 });
        const answer = await callLoopTool(name, msg.params?.arguments, bridge);
        result = { content: [{ type: 'text', text: answer.text }], ...(answer.isError ? { isError: true } : {}) };
      } else throw Object.assign(new Error(`unknown method ${msg.method}`), { code: -32601 });
      send({ jsonrpc: '2.0', id: msg.id, result });
    } catch (error) {
      send({ jsonrpc: '2.0', id: msg.id, error: { code: error?.code ?? -32603, message: String(error?.message ?? error) } });
    }
  };
  createInterface({ input: process.stdin, crlfDelay: Infinity }).on('line', (line) => {
    if (!line.trim()) return;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      send({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'parse error' } });
      return;
    }
    onMessage(msg);
  });
}

// A symlinked install (node resolves argv[1] through the link) must still be recognised as the
// entrypoint; an unresolvable argv[1] simply is not us.
const isEntrypoint = (() => {
  try {
    return realpathSync(process.argv[1]) === fileURLToPath(import.meta.url);
  } catch {
    return false;
  }
})();
if (isEntrypoint) serve(process.env);
