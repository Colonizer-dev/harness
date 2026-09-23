#!/usr/bin/env node
// Minimal MCP stdio server for the OpenCode module: the model sees colonizer_ask_user,
// colonizer_finding_file and colonizer_memory_propose, and every call is forwarded to the
// runner's loopback bridge (COLONIZER_BRIDGE_URL). Newline-delimited JSON-RPC 2.0; no dependencies.
// Asks can wait on a human for minutes, so while one is in flight the server sends periodic
// progress notifications on the call's progressToken to hold the request open.

import { createInterface } from 'node:readline';

const BRIDGE = process.env.COLONIZER_BRIDGE_URL ?? '';
const TOKEN = process.env.COLONIZER_BRIDGE_TOKEN ?? '';

const TOOLS = [
  {
    name: 'ask_user',
    description: 'Ask the user a question with 2-4 concrete options and wait for their answer. Use this whenever you need a decision, a clarification or any other input; never ask in plain text.',
    inputSchema: { type: 'object', properties: { questions: { type: 'array', items: { type: 'object', properties: { question: { type: 'string' }, header: { type: 'string' }, multiSelect: { type: 'boolean' }, options: { type: 'array', items: { type: 'object', properties: { label: { type: 'string' }, description: { type: 'string' } }, required: ['label'] } } }, required: ['question', 'options'] } } }, required: ['questions'] },
  },
  {
    name: 'finding_file',
    description: 'File a confirmed problem found outside the task (what is wrong, where, why it matters). Include how you confirmed it as evidence.',
    inputSchema: { type: 'object', properties: { title: { type: 'string' }, body: { type: 'string' }, evidence: { type: 'string' } }, required: ['title', 'body', 'evidence'] },
  },
  {
    name: 'memory_propose',
    description: 'Propose a durable, reusable learning for shared memory (scope repo, org or global). Nothing is written directly; the proposal goes to review.',
    inputSchema: { type: 'object', properties: { scope: { type: 'string', enum: ['repo', 'org', 'global'] }, title: { type: 'string' }, content: { type: 'string' }, tags: { type: 'array', items: { type: 'string' } } }, required: ['scope', 'title', 'content'] },
  },
];

const PATHS = { ask_user: '/ask', finding_file: '/finding', memory_propose: '/memory' };
const send = (msg) => process.stdout.write(`${JSON.stringify(msg)}\n`);

async function forward(path, args, progressToken) {
  let res;
  try {
    res = await fetch(`${BRIDGE}${path}`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${TOKEN}` }, body: JSON.stringify(args ?? {}) });
  } catch (error) {
    throw new Error(`colonizer bridge unreachable: ${error?.message ?? error}`);
  }
  if (!res.ok) throw new Error(`colonizer bridge answered HTTP ${res.status}`);
  if (progressToken === undefined) return res.json();
  // Hold a long ask open: a progress note every 15 s until the human answers.
  let elapsed = 0;
  const tick = setInterval(() => {
    elapsed += 15;
    send({ jsonrpc: '2.0', method: 'notifications/progress', params: { progressToken, progress: elapsed } });
  }, 15_000);
  try {
    return await res.json();
  } finally {
    clearInterval(tick);
  }
}

async function onCall(name, args, progressToken) {
  const path = PATHS[name];
  if (!path) throw Object.assign(new Error(`unknown tool ${name}`), { code: -32602 });
  try {
    const data = await forward(path, args, progressToken);
    if (data?.cancelled) return { content: [{ type: 'text', text: 'The question was cancelled before the user answered.' }], isError: true };
    if (data?.error) return { content: [{ type: 'text', text: String(data.error) }], isError: true };
    return { content: [{ type: 'text', text: typeof data === 'string' ? data : JSON.stringify(data) }] };
  } catch (error) {
    return { content: [{ type: 'text', text: String(error?.message ?? error) }], isError: true };
  }
}

async function onMessage(msg) {
  if (msg?.jsonrpc !== '2.0') return;
  if (msg.id === undefined) return; // notifications get no reply
  try {
    let result;
    if (msg.method === 'initialize') result = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'colonizer', version: '0.1.0' } };
    else if (msg.method === 'tools/list') result = { tools: TOOLS };
    else if (msg.method === 'tools/call') result = await onCall(msg.params?.name, msg.params?.arguments, msg.params?._meta?.progressToken);
    else throw Object.assign(new Error(`unknown method ${msg.method}`), { code: -32601 });
    send({ jsonrpc: '2.0', id: msg.id, result });
  } catch (error) {
    send({ jsonrpc: '2.0', id: msg.id, error: { code: error?.code ?? -32603, message: String(error?.message ?? error) } });
  }
}

const isEntrypoint = process.argv[1] === new URL(import.meta.url).pathname;
if (isEntrypoint) {
  createInterface({ input: process.stdin, crlfDelay: Infinity }).on('line', (line) => {
    if (!line.trim()) return;
    try {
      onMessage(JSON.parse(line));
    } catch {
      send({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'parse error' } });
    }
  });
}
