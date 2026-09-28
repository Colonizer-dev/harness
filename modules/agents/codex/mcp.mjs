#!/usr/bin/env node
// Minimal MCP stdio server for a Colonizer agent module, dependency-free: newline-delimited
// JSON-RPC 2.0 (protocolVersion 2024-11-05). `wait` and `memory_search` read the local filesystem
// here; `finding_file` and `memory_propose` leave the colony as protocol events, so they are
// forwarded to the runner's loopback bridge (COLONIZER_BRIDGE_URL). finding_file is offered only
// when the mothership set COLONIZER_FINDINGS=true, the memory tools only when COLONIZER_MEMORY_DIR
// is mounted; `wait` is always offered.

import { realpathSync } from 'node:fs';
import { open, readdir, readFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const BRIDGE = process.env.COLONIZER_BRIDGE_URL ?? '';
const TOKEN = process.env.COLONIZER_BRIDGE_TOKEN ?? '';
const MEMORY_DIR = process.env.COLONIZER_MEMORY_DIR ?? '';
const FINDINGS = process.env.COLONIZER_FINDINGS === 'true';

const TOOLS = [
  FINDINGS && {
    name: 'finding_file',
    description: 'File a confirmed problem found outside the task (what is wrong, where, why it matters). Include how you confirmed it as evidence.',
    inputSchema: {
      type: 'object',
      properties: {
        title: { type: 'string', description: 'One line, specific enough to find later: what is wrong, and where' },
        body: { type: 'string', description: 'Markdown: what is wrong, where (files and lines), why it matters, and a suggested fix if there is one' },
        evidence: { type: 'string', description: 'How the finding was confirmed: what you read or ran, and what it showed' },
      },
      required: ['title', 'body', 'evidence'],
    },
  },
  Boolean(MEMORY_DIR) && {
    name: 'memory_search',
    description: 'Search shared memory (notes from earlier colonies and the maintainer) for this repository, its GitHub organisation and globally. All terms must match; case-insensitive.',
    inputSchema: { type: 'object', properties: { query: { type: 'string', description: 'Space-separated terms; all must appear in a note' } }, required: ['query'] },
  },
  Boolean(MEMORY_DIR) && {
    name: 'memory_propose',
    description: 'Propose a durable, reusable learning for shared memory (scope repo, org or global). Nothing is written directly; the proposal goes to review. Never include secrets.',
    inputSchema: {
      type: 'object',
      properties: {
        scope: { type: 'string', enum: ['repo', 'org', 'global'], description: 'Whose memory: repo, org or global' },
        title: { type: 'string', description: 'One-line summary of the learning' },
        content: { type: 'string', description: 'The note itself: the learning, why it holds, how to apply it' },
        tags: { type: 'array', items: { type: 'string' }, description: 'Up to 10 short tags' },
      },
      required: ['scope', 'title', 'content'],
    },
  },
  {
    name: 'wait',
    description: 'Block until something finishes instead of polling. Pass exactly one of: seconds, to sleep; file and pattern (a JavaScript regex), to return as soon as a line of the file matches — the file need not exist yet; or pid, to return when that process is gone. One call replaces repeated greps on a build log, and on timeout it reports the file\'s tail so you can decide what happened.',
    inputSchema: {
      type: 'object',
      properties: {
        reason: { type: 'string', description: 'One line saying what you are waiting for; it keeps the wait legible in the transcript' },
        seconds: { type: 'number', description: 'Just sleep this long, then return' },
        file: { type: 'string', description: 'Path to watch; needs pattern. It need not exist yet — a log the build has not written is a normal case' },
        pattern: { type: 'string', description: 'JavaScript regular expression; needs file. Return as soon as a line of the file matches it' },
        pid: { type: 'integer', description: 'Return as soon as this process is gone' },
        timeout_seconds: { type: 'number', description: 'Cap for the file/pattern and pid waits (default 300, hard cap 1800)' },
      },
      required: ['reason'],
    },
  },
].filter(Boolean);

const send = (msg) => process.stdout.write(`${JSON.stringify(msg)}\n`);
const text = (value) => ({ content: [{ type: 'text', text: value }] });
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const secs = (ms) => `${Math.round(ms / 100) / 10} s`;

async function forward(path, args) {
  let res;
  try {
    res = await fetch(`${BRIDGE}${path}`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${TOKEN}` }, body: JSON.stringify(args ?? {}) });
  } catch (error) {
    throw new Error(`colonizer bridge unreachable: ${error?.message ?? error}`);
  }
  if (!res.ok) throw new Error(`colonizer bridge answered HTTP ${res.status}`);
  return res.json();
}

// --- shared memory search (docs/protocol.md §6.2) ----------------------------------------------

const MEMORY_SCOPES = ['repo', 'org', 'global'];

function noteTitle(text, file) {
  const heading = text.match(/^#{1,6}\s+(.+)$/m);
  if (heading) return heading[1].trim();
  const first = text.split('\n').find((line) => line.trim());
  return first ? first.trim().slice(0, 120) : file.replace(/\.md$/, '');
}

async function searchMemory(query) {
  const terms = String(query ?? '').toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return [];
  const results = [];
  for (const scope of MEMORY_SCOPES) {
    let files;
    try {
      files = (await readdir(join(MEMORY_DIR, scope, 'notes'))).filter((f) => f.endsWith('.md')).sort();
    } catch {
      continue; // scope not mounted
    }
    for (const file of files) {
      let note;
      try {
        note = await readFile(join(MEMORY_DIR, scope, 'notes', file), 'utf8');
      } catch {
        continue;
      }
      const lower = note.toLowerCase();
      if (!terms.every((term) => lower.includes(term))) continue;
      const at = Math.max(0, lower.indexOf(terms[0]));
      const start = Math.max(0, at - 120);
      const end = Math.min(note.length, at + terms[0].length + 120);
      const body = note.slice(start, end).replace(/\s+/g, ' ').trim();
      results.push({
        scope,
        title: noteTitle(note, file),
        file: `${scope}/notes/${file}`,
        snippet: `${start > 0 ? '…' : ''}${body}${end < note.length ? '…' : ''}`,
      });
      if (results.length >= 10) return results;
    }
  }
  return results;
}

// The notes are read from COLONIZER_MEMORY_DIR but named by their in-VM mount, the path the agent
// is told about (docs/protocol.md §6.2), so the format matches the Claude module's.
const formatResults = (results) =>
  results.length
    ? results.map((r) => `[${r.scope}] ${r.title} (/colonizer/memory/${r.file})\n${r.snippet}`).join('\n\n')
    : 'No shared memory matches that query.';

// --- wait (issue #181) -------------------------------------------------------------------------

const MAX_WAIT_SECONDS = 1800;
const DEFAULT_TIMEOUT_SECONDS = 300;
const POLL_MS = 250;
const MATCHING_LINE_CHARS = 500;
const TAIL_LINES = 20;
const TAIL_LINE_CHARS = 200;
// A timeout's tail diagnostic reads at most this much of the file, from the end.
const TAIL_BYTES = 64 * 1024;
// Progress output that never emits a newline (\r spinners) would grow the carried tail for the
// whole wait; keep only its last MiB, the part that can still match a live pattern.
const CARRY_MAX_BYTES = 1024 * 1024;
const USAGE = 'Pass exactly one of: seconds (a plain sleep), file and pattern (return when a line of the file matches), or pid (return when the process is gone).';
const clampNote = (asked) => (asked > MAX_WAIT_SECONDS ? `, asked for ${asked} s, capped at ${MAX_WAIT_SECONDS} s` : '');
const keepTail = (buffer) => (buffer.length > CARRY_MAX_BYTES ? Buffer.from(buffer.subarray(buffer.length - CARRY_MAX_BYTES)) : buffer);

/** What a non-regular path is, for the refusal text. */
const fileKind = (stats) =>
  stats.isDirectory() ? 'a directory'
  : stats.isFIFO() ? 'a FIFO (named pipe)'
  : stats.isSocket() ? 'a socket'
  : stats.isCharacterDevice() ? 'a character device'
  : stats.isBlockDevice() ? 'a block device'
  : 'not a regular file';

/** The file's new complete lines since the last poll, read incrementally from state.offset so a big
 * log is not re-read every round. The file need not exist yet; a truncation or an inode change
 * (rotated away) restarts from 0. The path is stat'ed before every open: opening a FIFO with no
 * writer would block in the threadpool, past the deadline, so a non-regular file is refused instead.
 * Returns { lines }, { badFile }, { error }, or null while the file has not appeared. */
async function newLines(path, state) {
  let stats;
  try {
    stats = await stat(path);
  } catch {
    return null; // not there yet
  }
  if (!stats.isFile()) return { badFile: fileKind(stats) };
  let handle;
  try {
    handle = await open(path, 'r');
  } catch (err) {
    if (err.code === 'ENOENT') return null; // deleted between the stat and the open: keep waiting
    return { error: err };
  }
  try {
    if (state.ino !== undefined && (stats.ino !== state.ino || stats.size < state.offset)) {
      state.offset = 0;
      state.carry = Buffer.alloc(0);
    }
    state.ino = stats.ino;
    if (stats.size === state.offset) return { lines: [] };
    const chunk = Buffer.alloc(stats.size - state.offset);
    const { bytesRead } = await handle.read(chunk, 0, chunk.length, state.offset);
    state.offset += bytesRead; // what was actually read, not what was asked: the file may have shrunk
    const buffer = Buffer.concat([state.carry, chunk]);
    const newline = buffer.lastIndexOf(0x0a);
    state.carry = keepTail(newline === -1 ? buffer : buffer.subarray(newline + 1));
    if (newline === -1) return { lines: [] };
    // The slice ends with \n, so the split's trailing '' is dropped rather than matched.
    return { lines: buffer.subarray(0, newline + 1).toString('utf8').split('\n').slice(0, -1) };
  } catch (err) {
    return { error: err };
  } finally {
    try {
      await handle.close();
    } catch {} // a close after a failed read must not mask that read's error
  }
}

/** Polls the file until a line matches. The still-growing last line is tested too, because a log's
 * final line usually has no newline yet; once it is newline-terminated the same bytes match above. */
async function waitForLine(file, regex, timeoutSeconds) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  const state = { offset: 0, ino: undefined, carry: Buffer.alloc(0) };
  for (;;) {
    const got = await newLines(file, state);
    if (got?.badFile) return { badFile: got.badFile };
    if (got?.error) return { failed: got.error };
    if (got) {
      for (const line of got.lines) {
        if (regex.test(line)) return { matched: line };
      }
      if (state.carry.length > 0 && regex.test(state.carry.toString('utf8'))) {
        return { matched: state.carry.toString('utf8') };
      }
    }
    if (Date.now() >= deadline) return { timedOut: true };
    await sleep(Math.max(0, Math.min(POLL_MS, deadline - Date.now())));
  }
}

/** The last lines of a file, for a timeout's diagnostic. Only the final TAIL_BYTES are read — a
 * stuck build's log can be arbitrarily large — and a missing file says so. */
async function fileTail(file) {
  let handle;
  try {
    handle = await open(file, 'r');
  } catch (err) {
    return err.code === 'ENOENT' ? 'The file never appeared.' : `The file could not be read: ${err.message}`;
  }
  try {
    const { size } = await handle.stat();
    const start = Math.max(0, size - TAIL_BYTES);
    const chunk = Buffer.alloc(size - start);
    let read = 0;
    while (read < chunk.length) {
      const { bytesRead } = await handle.read(chunk, read, chunk.length - read, start + read);
      if (!bytesRead) break; // the file shrank under the stat
      read += bytesRead;
    }
    let whole = chunk.subarray(0, read).toString('utf8');
    if (start > 0) {
      // The chunk usually starts mid-line; drop that fragment and admit the head is missing.
      const newline = whole.indexOf('\n');
      whole = newline === -1 ? '' : whole.slice(newline + 1);
    }
    const lines = whole.split('\n').filter((line) => line.trim());
    if (!lines.length) return `${file} exists but has no lines yet.`;
    const kept = lines.slice(-TAIL_LINES).map((line) => (line.length > TAIL_LINE_CHARS ? `${line.slice(0, TAIL_LINE_CHARS)}… [truncated]` : line));
    const head = lines.length > TAIL_LINES ? `Last ${kept.length} of ${lines.length} lines` : 'Last lines';
    const omitted = start > 0 ? ` (older lines omitted; only the last ${TAIL_BYTES / 1024} KiB were read)` : '';
    return `${head} of ${file}${omitted}:\n${kept.join('\n')}`;
  } catch (err) {
    return `The file could not be read: ${err.message}`;
  } finally {
    await handle.close().catch(() => {});
  }
}

/** `process.kill(pid, 0)` is the check: ESRCH means gone, EPERM means alive but not ours to signal. */
async function waitForExit(pid, timeoutSeconds) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  for (;;) {
    try {
      process.kill(pid, 0);
    } catch (err) {
      if (err.code === 'ESRCH') return { gone: true };
      if (err.code !== 'EPERM') return { uncheckable: err };
    }
    if (Date.now() >= deadline) return { timedOut: true };
    await sleep(Math.max(0, Math.min(POLL_MS, deadline - Date.now())));
  }
}

async function waitTool({ reason = '', seconds, file, pattern, pid, timeout_seconds }) {
  const started = Date.now();
  const waited = () => secs(Date.now() - started);
  const wants = { sleep: seconds !== undefined, line: file !== undefined || pattern !== undefined, exit: pid !== undefined };
  // Refusals come back as text, not a throw: the agent can read them and move on in the same turn.
  if (!String(reason).trim()) return text('Could not wait: reason is required — one line saying what you are waiting for.');
  if (wants.sleep + wants.line + wants.exit === 0) return text(`Could not wait: nothing to wait for. ${USAGE}`);
  if (wants.sleep + wants.line + wants.exit > 1) return text(`Could not wait: seconds, file+pattern and pid are alternatives; pass one. ${USAGE}`);
  if (wants.line && (file === undefined || pattern === undefined)) return text(`Could not wait: file and pattern go together. ${USAGE}`);
  for (const [name, value] of [['seconds', seconds], ['timeout_seconds', timeout_seconds]]) {
    if (value !== undefined && (!Number.isFinite(value) || value < 0)) return text(`Could not wait: ${name} must be a non-negative number of seconds.`);
  }
  if (wants.exit && (!Number.isInteger(pid) || pid < 1)) return text(`Could not wait: pid must be a process id (a positive integer).`);
  const timeoutSeconds = Math.min(timeout_seconds ?? DEFAULT_TIMEOUT_SECONDS, MAX_WAIT_SECONDS);

  if (wants.sleep) {
    await sleep(Math.min(seconds, MAX_WAIT_SECONDS) * 1000);
    return text(`Waited ${waited()} (${reason}${clampNote(seconds)}).`);
  }
  if (wants.line) {
    let regex;
    try {
      regex = new RegExp(String(pattern));
    } catch (err) {
      return text(`Could not wait: pattern is not a valid JavaScript regular expression (${err.message}).`);
    }
    const result = await waitForLine(file, regex, timeoutSeconds);
    // A refused wait is text, not a throw: the agent can read it and move on in the same turn.
    if (result.badFile) return text(`Could not wait: ${file} is ${result.badFile}; only a regular file can be watched.`);
    if (result.failed) return text(`Could not wait: ${file} could not be read (${result.failed.message}).`);
    if (result.timedOut) return text(`Timed out after ${waited()} waiting for ${regex} in ${file} (${reason}${clampNote(timeout_seconds)}).\n${await fileTail(file)}`);
    const line = result.matched.length > MATCHING_LINE_CHARS ? `${result.matched.slice(0, MATCHING_LINE_CHARS)}… [truncated]` : result.matched;
    return text(`Matched ${regex} in ${file} after ${waited()} (${reason}${clampNote(timeout_seconds)}).\nMatching line: ${line}`);
  }
  const result = await waitForExit(pid, timeoutSeconds);
  if (result.uncheckable) return text(`Could not wait: cannot check pid ${pid} (${result.uncheckable.message}).`);
  const verdict = result.gone ? `Process ${pid} is gone` : `Process ${pid} was still running`;
  return text(`${verdict} after ${waited()} (${reason}${clampNote(timeout_seconds)}).`);
}

// --- JSON-RPC over stdio -----------------------------------------------------------------------

async function onCall(name, args) {
  if (!TOOLS.some((tool) => tool.name === name)) throw Object.assign(new Error(`unknown tool ${name}`), { code: -32602 });
  try {
    if (name === 'wait') return await waitTool(args ?? {});
    if (name === 'memory_search') return text(formatResults(await searchMemory(args?.query)));
    const data = await forward(name === 'finding_file' ? '/finding' : '/memory', args);
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
    else if (msg.method === 'tools/call') result = await onCall(msg.params?.name, msg.params?.arguments);
    else throw Object.assign(new Error(`unknown method ${msg.method}`), { code: -32601 });
    send({ jsonrpc: '2.0', id: msg.id, result });
  } catch (error) {
    send({ jsonrpc: '2.0', id: msg.id, error: { code: error?.code ?? -32603, message: String(error?.message ?? error) } });
  }
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
