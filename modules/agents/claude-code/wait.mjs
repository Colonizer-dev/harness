// Waiting, runner side (issue #181). A colony had no way to wait, so it burned model turns polling:
// repeated greps on a build log, `Bash true` with the description "Idle", even a whole subagent as a
// sleep. One blocking call replaces the poll loop and costs no model turn.

import { open, stat } from 'node:fs/promises';

export const WAIT_SERVER = 'colonizer_wait';
export const WAIT_TOOL = `mcp__${WAIT_SERVER}__wait`;

export const WAIT_PROMPT_APPEND = [
  '- When the only thing left is a long build, test suite or background subagent, call mcp__colonizer_wait__wait: it blocks until a log line matches, a process is gone or the seconds pass, and costs no model turn.',
  '- Do not poll a log with repeated greps, do not burn a turn on Bash true, and do not start a subagent just to sleep; one wait call replaces the loop.',
].join('\n');

// A wait holds a turn, so 30 minutes is the most any of them will.
const MAX_WAIT_SECONDS = 1800;
const DEFAULT_TIMEOUT_SECONDS = 300;
const POLL_MS = 250;
const MATCHING_LINE_CHARS = 500;
const TAIL_LINES = 20;
const TAIL_LINE_CHARS = 200;
// Progress output that never emits a newline (\r spinners) would grow the carried tail for the whole
// wait; keep only its last MiB, which is the part that can still match a live pattern.
const CARRY_MAX_BYTES = 1024 * 1024;
// A timeout's tail diagnostic reads at most this much of the file, from the end.
const TAIL_BYTES = 64 * 1024;

const USAGE =
  'Pass exactly one of: seconds (a plain sleep), file and pattern (return when a line of the file matches), or pid (return when the process is gone).';
// A pid wait answers "has it stopped existing", not "did it succeed": only a parent can reap a child.
const REAP_NOTE =
  'This reports the process disappearing, not its exit status: the colony cannot reap a process it did not spawn.';

const text = (value) => ({ content: [{ type: 'text', text: value }] });
const secs = (ms) => `${Math.round(ms / 100) / 10} s`;

const sleep = (ms, signal) =>
  new Promise((resolve) => {
    if (signal?.aborted) return resolve();
    const timer = setTimeout(finish, ms);
    const onAbort = () => finish();
    function finish() {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }
    signal?.addEventListener('abort', onAbort, { once: true });
  });

/** What a wait says about a value over the cap, or '' when it was not. */
const clampNote = (asked) => (asked > MAX_WAIT_SECONDS ? `, asked for ${asked} s, capped at ${MAX_WAIT_SECONDS} s` : '');

/** What a non-regular path is, for the refusal text. */
const fileKind = (stats) =>
  stats.isDirectory() ? 'a directory'
  : stats.isFIFO() ? 'a FIFO (named pipe)'
  : stats.isSocket() ? 'a socket'
  : stats.isCharacterDevice() ? 'a character device'
  : stats.isBlockDevice() ? 'a block device'
  : 'not a regular file';

// Keep at most the tail of an unterminated line, copied loose from the poll's full read so the big
// buffer is not pinned for the life of the wait.
const keepTail = (buffer) => (buffer.length > CARRY_MAX_BYTES ? Buffer.from(buffer.subarray(buffer.length - CARRY_MAX_BYTES)) : buffer);

/**
 * The file's new complete lines since the last poll. The file need not exist yet — waiting for a log
 * that has not been created is the normal case. It is stat'ed before every open: opening a FIFO with
 * no writer blocks inside the threadpool, never reaching the deadlines or the abort checks, so a
 * path that exists and is not a regular file is refused instead. Reads incrementally from the last
 * byte offset so a big log is not re-read each poll, and starts over when the file shrinks or its
 * inode changes (truncated, or rotated away under the same path); a same-inode rewrite that leaves
 * the file at least as long as the offset already read cannot be detected — the watcher assumes the
 * file is appended to. `carry` holds the still-growing tail between polls, trimmed to its last
 * CARRY_MAX_BYTES.
 * Returns the new complete lines (possibly none), `{ badFile }` for a non-regular path, `{ error }`
 * for a read failure, or null while the file has not appeared.
 */
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
    if (state.inode !== undefined && stats.ino !== state.inode) {
      state.offset = 0;
      state.carry = Buffer.alloc(0);
    }
    state.inode = stats.ino;
    const { size } = stats;
    if (size < state.offset) {
      state.offset = 0;
      state.carry = Buffer.alloc(0);
    }
    if (size === state.offset) return [];
    const chunk = Buffer.alloc(size - state.offset);
    const { bytesRead } = await handle.read(chunk, 0, chunk.length, state.offset);
    state.offset += bytesRead; // what was actually read, not what was asked: the file may have shrunk
    const buffer = Buffer.concat([state.carry, chunk]);
    const newline = buffer.lastIndexOf(0x0a);
    if (newline === -1) {
      state.carry = keepTail(buffer);
      return [];
    }
    state.carry = keepTail(buffer.subarray(newline + 1));
    // The slice ends with \n, so the split's trailing '' is dropped rather than matched.
    return buffer.subarray(0, newline + 1).toString('utf8').split('\n').slice(0, -1);
  } catch (err) {
    return { error: err };
  } finally {
    try {
      await handle.close();
    } catch {} // a close after a failed read must not mask that read's error
  }
}

async function waitForLine(path, regex, { timeoutSeconds, signal }) {
  const state = { offset: 0, inode: undefined, carry: Buffer.alloc(0) };
  const deadline = Date.now() + timeoutSeconds * 1000;
  for (;;) {
    if (signal?.aborted) return { aborted: true };
    const got = await newLines(path, state);
    if (got?.badFile) return { badFile: got.badFile };
    if (got?.error) return { failed: got.error };
    if (got) {
      for (const line of got) {
        if (regex.test(line)) return { matched: line };
      }
      // The unterminated tail is tested too, because a log's last line usually has no newline yet
      // and only a grep-poll would still see it. Returning on the first hit means a line is never
      // reported twice; once that tail is newline-terminated, the same bytes match above.
      if (state.carry.length > 0) {
        const tail = state.carry.toString('utf8');
        if (regex.test(tail)) return { matched: tail, partial: true };
      }
    }
    if (Date.now() >= deadline) return { timedOut: true };
    await sleep(Math.min(POLL_MS, deadline - Date.now()), signal);
  }
}

/** `process.kill(pid, 0)` is the check: ESRCH means gone, EPERM means alive but not ours to signal. */
async function waitForExit(pid, { timeoutSeconds, signal }) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  for (;;) {
    if (signal?.aborted) return { aborted: true };
    try {
      process.kill(pid, 0);
    } catch (err) {
      if (err.code === 'ESRCH') return { gone: true };
      if (err.code !== 'EPERM') return { uncheckable: err };
    }
    if (Date.now() >= deadline) return { timedOut: true };
    await sleep(Math.min(POLL_MS, deadline - Date.now()), signal);
  }
}

/** The last lines of a file, for a timeout's diagnostic. Only the final TAIL_BYTES are read — a
 * stuck build's log can be arbitrarily large — and a missing file says so, while any other failure
 * names itself rather than being passed off as "never appeared". */
async function fileTail(path) {
  let handle;
  try {
    handle = await open(path, 'r');
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
    if (!lines.length) return `${path} exists but has no lines yet.`;
    const kept = lines.slice(-TAIL_LINES).map((line) => (line.length > TAIL_LINE_CHARS ? `${line.slice(0, TAIL_LINE_CHARS)}… [truncated]` : line));
    const head = lines.length > TAIL_LINES ? `Last ${kept.length} of ${lines.length} lines` : 'Last lines';
    const omitted = start > 0 ? ` (older lines omitted; only the last ${TAIL_BYTES / 1024} KiB were read)` : '';
    return `${head} of ${path}${omitted}:\n${kept.join('\n')}`;
  } catch (err) {
    return `The file could not be read: ${err.message}`;
  } finally {
    await handle.close().catch(() => {});
  }
}

/**
 * Builds the in-process MCP server with wait.
 * The SDK helpers and zod are injected so tests can run without the SDK transport.
 */
export function createWaitServer({ createSdkMcpServer, tool, z }) {
  const wait = tool(
    'wait',
    'Block until something finishes instead of polling. Pass exactly one of: seconds, to sleep; file and pattern (a JavaScript regex), to return as soon as a line of the file matches — the file need not exist yet; or pid, to return when that process is gone. Use it for a long build, test suite or background subagent: one call replaces repeated greps on a log and placeholder Bash true calls, and on timeout it reports the file\'s tail so you can decide what happened.',
    {
      reason: z.string().min(1).max(200).describe('One line saying what you are waiting for; it keeps the wait legible in the transcript'),
      seconds: z.number().optional().describe('Just sleep this long, then return'),
      file: z.string().optional().describe('Path to watch; needs pattern. It need not exist yet — a log the build has not written is a normal case'),
      pattern: z.string().optional().describe('JavaScript regular expression; needs file. Return as soon as a line of the file matches it'),
      pid: z.number().int().optional().describe('Return as soon as this process is gone'),
      timeout_seconds: z.number().optional().describe('Cap for the file/pattern and pid waits (default 300, hard cap 1800)'),
    },
    async ({ reason, seconds, file, pattern, pid, timeout_seconds }, extra) => {
      const signal = extra?.signal;
      const started = Date.now();
      const waited = () => secs(Date.now() - started);
      const wants = { sleep: seconds !== undefined, line: file !== undefined || pattern !== undefined, exit: pid !== undefined };
      const chosen = Object.values(wants).filter(Boolean).length;

      // Semantic errors come back as text rather than throwing: a refused wait must not cost the
      // agent a retry loop any more than a timed-out one should.
      if (chosen === 0) return text(`Could not wait: nothing to wait for. ${USAGE}`);
      if (chosen > 1) {
        if (wants.sleep) return text(`Could not wait: seconds is a plain sleep and cannot be combined with ${wants.line ? 'file/pattern' : 'pid'}. ${USAGE}`);
        return text(`Could not wait: file+pattern and pid are alternatives; pass one. ${USAGE}`);
      }
      if (wants.line && file === undefined) return text(`Could not wait: pattern needs file — the path whose lines it should match. ${USAGE}`);
      if (wants.line && pattern === undefined) return text(`Could not wait: file needs pattern — the JavaScript regex a line should match. ${USAGE}`);
      for (const [name, value] of [['seconds', seconds], ['timeout_seconds', timeout_seconds]]) {
        if (value !== undefined && !Number.isFinite(value)) return text(`Could not wait: ${name} must be a number of seconds.`);
        if (value < 0) return text(`Could not wait: ${name} cannot be negative.`);
      }
      if (wants.exit && (!Number.isInteger(pid) || pid < 1)) return text(`Could not wait: pid must be a process id (a positive integer); ${pid} would not address a process.`);
      let regex;
      if (wants.line) {
        try {
          regex = new RegExp(pattern);
        } catch (err) {
          return text(`Could not wait: pattern is not a valid JavaScript regular expression (${err.message}).`);
        }
      }

      const askedSeconds = seconds;
      const askedTimeout = timeout_seconds ?? DEFAULT_TIMEOUT_SECONDS;
      const timeoutSeconds = Math.min(askedTimeout, MAX_WAIT_SECONDS);

      if (wants.sleep) {
        const capped = Math.min(seconds, MAX_WAIT_SECONDS);
        await sleep(capped * 1000, signal);
        if (signal?.aborted) return text(`Wait aborted after ${waited()} (${reason}${clampNote(askedSeconds)}).`);
        return text(`Waited ${waited()} (${reason}${clampNote(askedSeconds)}).`);
      }

      if (wants.line) {
        const result = await waitForLine(file, regex, { timeoutSeconds, signal });
        // A refused wait is text, not a throw: the agent can read it and move on in the same turn.
        if (result.badFile) return text(`Could not wait: ${file} is ${result.badFile}; only a regular file can be watched.`);
        if (result.failed) return text(`Could not wait: ${file} could not be read (${result.failed.message}).`);
        if (result.aborted) return text(`Wait aborted after ${waited()} (${reason}${clampNote(askedTimeout)}).`);
        if (result.matched !== undefined) {
          const line =
            result.matched.length > MATCHING_LINE_CHARS
              ? `${result.matched.slice(0, MATCHING_LINE_CHARS)}… [truncated]`
              : result.matched;
          const partialNote = result.partial ? '\nThe matching line has no newline yet, so it may still be growing.' : '';
          return text(`Matched ${regex} in ${file} after ${waited()} (${reason}${clampNote(askedTimeout)}).\nMatching line: ${line}${partialNote}`);
        }
        // The timeout result has to be the whole story: the agent should not need another call to
        // see where the build actually is.
        return text(`Timed out after ${waited()} waiting for ${regex} in ${file} (${reason}${clampNote(askedTimeout)}).\n${await fileTail(file)}`);
      }

      const result = await waitForExit(pid, { timeoutSeconds, signal });
      if (result.uncheckable) return text(`Could not wait: cannot check pid ${pid} (${result.uncheckable.message}).`);
      if (result.aborted) return text(`Wait aborted after ${waited()} (${reason}${clampNote(askedTimeout)}).`);
      if (result.gone) return text(`Process ${pid} is gone after ${waited()} (${reason}${clampNote(askedTimeout)}). ${REAP_NOTE}`);
      return text(`Process ${pid} was still running after ${waited()} (${reason}${clampNote(askedTimeout)}). ${REAP_NOTE}`);
    },
  );
  return createSdkMcpServer({ name: WAIT_SERVER, version: '1.0.0', tools: [wait] });
}
