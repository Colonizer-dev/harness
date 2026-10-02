#!/usr/bin/env node
// Shared memory's read tools for the runners without an in-process MCP server (issue #766):
// memory_briefing, memory_changes and memory_search, over the mounted store in
// COLONIZER_MEMORY_DIR. The logic is memory.mjs, a byte-identical copy of the claude-code module's;
// this file only describes the tools and serves them.
//
// Imported by the OpenCode module's mcp.mjs and the Pi module's memory extension, and run as a
// dependency-free stdio MCP server (newline-delimited JSON-RPC 2.0, protocolVersion 2024-11-05)
// that the ACP runner registers on its session. One file in three modules: acp holds the
// original, opencode and pi copies that a test keeps byte-identical.
//
// Memory is pulled, never injected: nothing here reaches a prompt. Every answer is framed by
// memory.mjs as sourced data to verify, and a revoked note is gone from the next answer because
// the mothership rewrites notes.json the moment it is revoked.

import { realpathSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

import { briefing, changes, formatResults, MEMORY_PROMPT_APPEND, memoryState, searchMemory } from './memory.mjs';

export { MEMORY_PROMPT_APPEND, memoryState };

/** The read tools, as JSON Schema: MCP's inputSchema, and the parameters a Pi tool takes. */
export const MEMORY_READ_TOOLS = [
  {
    name: 'memory_briefing',
    description: 'A short, sourced summary of shared memory (plans, decisions, file-change notes, failures, architecture notes, conventions) for this repository, its GitHub organisation and globally. Pass a topic to narrow it. Each entry names its source: colony, repository and commit.',
    inputSchema: { type: 'object', properties: { topic: { type: 'string', description: 'Words that must all appear in an entry; omit for everything' } } },
  },
  {
    name: 'memory_changes',
    description: 'What changed in shared memory since you last asked (or since an ISO time): entries added, and entries revoked or removed, which you should stop relying on.',
    inputSchema: { type: 'object', properties: { since: { type: 'string', description: 'ISO 8601 time; omit for "since I last asked"' } } },
  },
  {
    name: 'memory_search',
    description: 'Search shared memory (notes from earlier colonies and the maintainer) for this repository, its GitHub organisation and globally. All terms must match; case-insensitive.',
    inputSchema: { type: 'object', properties: { query: { type: 'string', description: 'Space-separated terms; all must appear in a note' } }, required: ['query'] },
  },
];

export const MEMORY_READ_TOOL_NAMES = MEMORY_READ_TOOLS.map((tool) => tool.name);

/** One read tool's answer as text. `state` is the colony's, so memory_changes knows what it was told. */
export async function callMemoryTool(name, args, { dir, state }) {
  const input = args && typeof args === 'object' ? args : {};
  if (name === 'memory_briefing') return briefing(dir, { topic: typeof input.topic === 'string' ? input.topic : undefined, state });
  if (name === 'memory_changes') return changes(dir, { since: typeof input.since === 'string' && input.since ? input.since : undefined, state });
  if (name === 'memory_search') return formatResults(await searchMemory(dir, input.query));
  throw new Error(`unknown memory tool ${name}`);
}

// --- stdio MCP server --------------------------------------------------------------------------

function serve(dir) {
  const state = memoryState();
  const send = (msg) => process.stdout.write(`${JSON.stringify(msg)}\n`);
  const onMessage = async (msg) => {
    if (msg?.jsonrpc !== '2.0' || msg.id === undefined) return; // notifications get no reply
    try {
      let result;
      if (msg.method === 'initialize') result = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'colonizer_memory', version: '0.1.0' } };
      else if (msg.method === 'tools/list') result = { tools: dir ? MEMORY_READ_TOOLS : [] };
      else if (msg.method === 'tools/call') {
        const name = msg.params?.name;
        if (!dir || !MEMORY_READ_TOOL_NAMES.includes(name)) throw Object.assign(new Error(`unknown tool ${name}`), { code: -32602 });
        try {
          result = { content: [{ type: 'text', text: await callMemoryTool(name, msg.params?.arguments, { dir, state }) }] };
        } catch (error) {
          result = { content: [{ type: 'text', text: String(error?.message ?? error) }], isError: true };
        }
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
if (isEntrypoint) serve(process.env.COLONIZER_MEMORY_DIR ?? '');
