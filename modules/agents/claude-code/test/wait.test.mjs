import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { appendFile, chmod, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { z } from 'zod';

import { buildOptions as buildOptionsWithDefaults } from '../runner.mjs';
import { createWaitServer, WAIT_PROMPT_APPEND, WAIT_SERVER, WAIT_TOOL } from '../wait.mjs';

// Real timers with small durations: a wait that cannot survive a 250 ms poll is not worth faking a
// clock for. Nothing here should take more than about a second.
const waitServerOf = () => {
  let def;
  const server = createWaitServer({ createSdkMcpServer: (options) => options, tool: (name, description, shape, handler) => (def = { name, description, shape, handler }), z });
  return { server, ...def };
};

const textOf = (reply) => reply.content[0].text;

async function tempFile(name, contents = '') {
  const dir = await mkdtemp(join(tmpdir(), 'colonizer-wait-'));
  const path = join(dir, name);
  if (contents) await writeFile(path, contents);
  return { dir, path };
}

test('wait is one tool on its own server, and the description carries the anti-polling guidance', () => {
  const { server, name, description } = waitServerOf();
  assert.equal(server.name, WAIT_SERVER);
  assert.equal(name, 'wait');
  assert.equal(WAIT_TOOL, 'mcp__colonizer_wait__wait');
  assert.match(description, /instead of polling/);
  assert.match(description, /repeated greps on a log and placeholder Bash true calls/, 'subagents see descriptions, not the system prompt');
  assert.deepEqual(Object.keys(waitServerOf().shape), ['reason', 'seconds', 'file', 'pattern', 'pid', 'timeout_seconds']);
});

test('the schema demands a reason and a whole pid', () => {
  const { shape } = waitServerOf();
  const schema = z.object(shape);
  assert.equal(schema.safeParse({ reason: 'build' }).success, true);
  assert.equal(schema.safeParse({}).success, false, 'the reason is what keeps a wait legible in the transcript');
  assert.equal(schema.safeParse({ reason: 'x'.repeat(201) }).success, false);
  assert.equal(schema.safeParse({ reason: 'build', pid: 1.5 }).success, false);
});

test('seconds sleeps roughly that long and says what it waited for', async () => {
  const { handler } = waitServerOf();
  const started = Date.now();
  const reply = await handler({ reason: 'a short pause', seconds: 0.2 });
  assert.ok(Date.now() - started >= 150, 'the sleep actually blocks');
  assert.match(textOf(reply), /Waited 0\.2 s \(a short pause\)\./);
});

test('a pattern already in the file returns at once with the matching line', async () => {
  const { dir, path } = await tempFile('build.log', 'step 1 ok\ntest result: ok. 42 passed\nstep 2 running\n');
  try {
    const { handler } = waitServerOf();
    const started = Date.now();
    const reply = await handler({ reason: 'suite finished', file: path, pattern: 'test result: ok' });
    assert.ok(Date.now() - started < 1000);
    assert.match(textOf(reply), /Matched \/test result: ok\/ in/);
    assert.match(textOf(reply), /after 0(\.\d)? s \(suite finished\)/);
    assert.match(textOf(reply), /Matching line: test result: ok\. 42 passed/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a pattern that appears after the call starts still matches', async () => {
  const { dir, path } = await tempFile('build.log', 'compiling…\n');
  try {
    setTimeout(() => appendFile(path, 'all tests passed\n').catch(() => {}), 80);
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'tests to pass', file: path, pattern: 'all tests passed', timeout_seconds: 3 });
    assert.match(textOf(reply), /Matched \/all tests passed\//);
    assert.match(textOf(reply), /Matching line: all tests passed/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a pattern spanning two poll cycles is joined by the carried tail, not lost', async () => {
  const { dir, path } = await tempFile('chunked.log', 'test result');
  try {
    // The completion lands between polls, so no single read ever contains the whole line: only the
    // tail carried over from the first poll can complete it. This fails if the carry is dropped.
    setTimeout(() => appendFile(path, ' : ok\n').catch(() => {}), 300);
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'suite finished', file: path, pattern: 'test result : ok', timeout_seconds: 3 });
    assert.match(textOf(reply), /Matched \/test result : ok\//);
    assert.match(textOf(reply), /Matching line: test result : ok/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a multi-byte character split across two reads is reassembled, not mangled', async () => {
  // "caf" plus the first byte of é; the second byte and the newline arrive a poll later.
  const { dir, path } = await tempFile('utf8.log', Buffer.from([0x63, 0x61, 0x66, 0xc3]));
  try {
    setTimeout(() => appendFile(path, Buffer.from([0xa9, 0x0a])).catch(() => {}), 300);
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'cafe done', file: path, pattern: 'café', timeout_seconds: 3 });
    assert.match(textOf(reply), /Matching line: café/);
    assert.ok(!textOf(reply).includes('�'), 'the split character is whole, not a replacement char');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a final line with no trailing newline still matches, flagged as unterminated', async () => {
  const { dir, path } = await tempFile('unterminated.log', 'step 1 ok\ntest result: ok');
  try {
    const { handler } = waitServerOf();
    const started = Date.now();
    const reply = await handler({ reason: 'suite finished', file: path, pattern: 'test result: ok', timeout_seconds: 2 });
    assert.ok(Date.now() - started < 1000, 'the unterminated tail is tested every poll, not only at the end');
    assert.match(textOf(reply), /Matched \/test result: ok\//);
    assert.match(textOf(reply), /Matching line: test result: ok/);
    assert.match(textOf(reply), /no newline yet/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a file that does not exist yet is waited for, not an error', async () => {
  const { dir, path } = await tempFile('late.log');
  try {
    setTimeout(() => writeFile(path, 'build done\n').catch(() => {}), 80);
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'the build', file: path, pattern: 'build done', timeout_seconds: 3 });
    assert.match(textOf(reply), /Matched \/build done\//);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a log that shrinks under the wait (truncated or rotated) is re-read from the start', async () => {
  const { dir, path } = await tempFile('rotated.log', 'a'.repeat(5000));
  try {
    // The rewrite is shorter than the offset already read, and READY sits inside the region a
    // reader that never restarts would skip forever.
    setTimeout(() => writeFile(path, 'b'.repeat(3000) + '\nREADY\n').catch(() => {}), 80);
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'restart marker', file: path, pattern: 'READY', timeout_seconds: 3 });
    assert.match(textOf(reply), /Matched \/READY\//);
    assert.match(textOf(reply), /Matching line: READY/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a pattern wait that times out reports the tail of the file', async () => {
  const { dir, path } = await tempFile('build.log', 'warning: slow\nstill compiling\nno result yet\n');
  try {
    const { handler } = waitServerOf();
    const started = Date.now();
    const reply = await handler({ reason: 'never arriving', file: path, pattern: 'test result', timeout_seconds: 0.4 });
    assert.ok(Date.now() - started >= 400, 'the timeout actually bounds the wait');
    assert.match(textOf(reply), /Timed out after 0\.\d s waiting for \/test result\//);
    assert.match(textOf(reply), /Last lines of/);
    assert.match(textOf(reply), /still compiling/, 'the tail is diagnostic enough that no second call is needed');
    assert.ok(!textOf(reply).includes('compiling…'), 'lines before the tail are not dragged in');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a matching line over 500 characters is truncated in the result', async () => {
  const { dir, path } = await tempFile('long.log', `${'x'.repeat(600)} NEEDLE\n`);
  try {
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'long line', file: path, pattern: 'NEEDLE', timeout_seconds: 2 });
    assert.match(textOf(reply), /Matched \/NEEDLE\//);
    const shown = textOf(reply).split('\n').find((line) => line.startsWith('Matching line:'));
    assert.equal(shown, `Matching line: ${'x'.repeat(500)}… [truncated]`);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a tail line over 200 characters is truncated in the timeout report', async () => {
  const { dir, path } = await tempFile('tail.log', `${'a'.repeat(300)}\nlast words\n`);
  try {
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'never arriving', file: path, pattern: 'test result', timeout_seconds: 0.3 });
    assert.match(textOf(reply), /Timed out after/);
    const tail = textOf(reply).split('\n').find((line) => line.startsWith('aaaa'));
    assert.equal(tail, `${'a'.repeat(200)}… [truncated]`);
    assert.match(textOf(reply), /last words/, 'a short line after a truncated one still shows whole');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a timeout on a file that never appeared says so', async () => {
  const { dir, path } = await tempFile('ghost.log');
  try {
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'a log nothing writes', file: path, pattern: 'done', timeout_seconds: 0.3 });
    assert.match(textOf(reply), /Timed out after/);
    assert.match(textOf(reply), /The file never appeared\./);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('an existing but unreadable file is a read failure, not a missing file', { timeout: 5000 }, async (t) => {
  if (process.platform === 'win32') return t.skip('chmod 0000 does not make a file unreadable on Windows');
  if (typeof process.getuid === 'function' && process.getuid() === 0) return t.skip('root can read a 0000 file');
  const { dir, path } = await tempFile('secret.log', 'hidden results\n');
  try {
    await chmod(path, 0o000);
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'permission trouble', file: path, pattern: 'hidden', timeout_seconds: 2 });
    assert.match(textOf(reply), /Could not wait: .*could not be read/);
    assert.ok(!textOf(reply).includes('never appeared'), 'the file exists, so "never appeared" would be a lie');
  } finally {
    await chmod(path, 0o644).catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
});

test('a directory path returns a text refusal instead of throwing EISDIR', async () => {
  const { dir } = await tempFile('unused');
  try {
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'wrong kind of path', file: dir, pattern: 'x', timeout_seconds: 2 });
    assert.match(textOf(reply), /Could not wait: .* is a directory/);
    assert.ok(!textOf(reply).includes('EISDIR'), 'the failure is explained, not dumped');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a FIFO is refused promptly instead of blocking open past every timeout', { timeout: 5000 }, async (t) => {
  const { dir, path } = await tempFile('pipe.fifo');
  try {
    const made = await new Promise((resolve) => execFile('mkfifo', [path], (err) => resolve(!err)));
    if (!made) return t.skip('mkfifo is not available on this platform');
    const { handler } = waitServerOf();
    const started = Date.now();
    const reply = await handler({ reason: 'wrong kind of path', file: path, pattern: 'x', timeout_seconds: 2 });
    assert.ok(Date.now() - started < 3000, 'the refusal does not sit in a blocked open');
    assert.match(textOf(reply), /Could not wait: .*FIFO/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('rotation by rename (a new inode, no shrink) restarts the read', async () => {
  const { dir, path } = await tempFile('rotated.log', 'x'.repeat(4096) + '\n');
  try {
    // The replacement is bigger than the old file, so the shrink check cannot fire, and READY sits
    // before the offset already read, so a reader that kept going would never see it. Only the
    // changed inode can restart the read from the start.
    setTimeout(async () => {
      const next = join(dir, 'next.log');
      await writeFile(next, 'y'.repeat(3000) + '\nREADY\n' + 'y'.repeat(4096)).catch(() => {});
      await rename(next, path).catch(() => {});
    }, 300);
    const { handler } = waitServerOf();
    const reply = await handler({ reason: 'restart marker', file: path, pattern: 'READY', timeout_seconds: 3 });
    assert.match(textOf(reply), /Matched \/READY\//);
    assert.match(textOf(reply), /Matching line: READY/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('timeout_seconds: 0 times out at once instead of waiting a poll', async () => {
  const { handler } = waitServerOf();
  const reply = await handler({ reason: 'instant', file: 'never-written.log', pattern: 'done', timeout_seconds: 0 });
  assert.match(textOf(reply), /Timed out after 0(\.\d)? s/);
  assert.match(textOf(reply), /The file never appeared\./);
  assert.match(textOf(await handler({ reason: 'no pause', seconds: 0 })), /Waited 0 s/);
});

test('a pid wait returns when a real child process is gone', async () => {
  const child = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 300)'], { stdio: 'ignore' });
  const { handler } = waitServerOf();
  const started = Date.now();
  const reply = await handler({ reason: 'helper subprocess', pid: child.pid });
  assert.ok(Date.now() - started >= 200, 'a live process is waited out, not reported gone immediately');
  assert.match(textOf(reply), /Process \d+ is gone after 0(\.\d+)? s \(helper subprocess\)/);
  assert.match(textOf(reply), /not its exit status/, 'the colony cannot reap a process it did not spawn');
});

test('a pid wait that times out says the process was still running', async () => {
  const { handler } = waitServerOf();
  const reply = await handler({ reason: 'this very test process', pid: process.pid, timeout_seconds: 0.3 });
  assert.match(textOf(reply), /Process \d+ was still running after 0\.\d s/);
  assert.match(textOf(reply), /not its exit status/);
});

test('a wait that is not exactly one condition is refused with what to pass instead', async () => {
  const { handler } = waitServerOf();
  const refused = async (input) => textOf(await handler(input));
  assert.match(await refused({ reason: 'x' }), /nothing to wait for\. Pass exactly one of/);
  assert.match(await refused({ reason: 'x', seconds: 1, file: 'a.log', pattern: 'b' }), /seconds is a plain sleep and cannot be combined with file\/pattern/);
  assert.match(await refused({ reason: 'x', seconds: 1, pid: 2 }), /seconds is a plain sleep and cannot be combined with pid/);
  assert.match(await refused({ reason: 'x', file: 'a.log', pattern: 'b', pid: 2 }), /file\+pattern and pid are alternatives/);
  assert.match(await refused({ reason: 'x', file: 'a.log' }), /file needs pattern/);
  assert.match(await refused({ reason: 'x', pattern: 'done' }), /pattern needs file/);
  assert.match(await refused({ reason: 'x', seconds: -5 }), /seconds cannot be negative/);
  assert.match(await refused({ reason: 'x', file: 'a.log', pattern: 'b', timeout_seconds: -1 }), /timeout_seconds cannot be negative/);
  assert.match(await refused({ reason: 'x', seconds: 'soon' }), /seconds must be a number of seconds/);
  assert.match(await refused({ reason: 'x', pid: 0 }), /pid must be a process id \(a positive integer\)/);
});

test('an invalid pattern names the regex problem instead of throwing', async () => {
  const { handler } = waitServerOf();
  const reply = await handler({ reason: 'x', file: 'a.log', pattern: '(unclosed' });
  assert.match(textOf(reply), /pattern is not a valid JavaScript regular expression/);
  assert.match(textOf(reply), /Unterminated group/);
});

test('values over the 30-minute cap are clamped and said so, not refused', async () => {
  const { handler } = waitServerOf();

  // A seconds wait over the cap is clamped before it sleeps; aborting it keeps the test to milliseconds.
  const controller = new AbortController();
  setTimeout(() => controller.abort(), 30);
  const abortedSleep = await handler({ reason: 'too long to really wait', seconds: 2000 }, { signal: controller.signal });
  assert.match(textOf(abortedSleep), /asked for 2000 s, capped at 1800 s/);

  // A timeout cap is visible even when the pattern matches immediately.
  const { dir, path } = await tempFile('quick.log', 'done\n');
  try {
    const reply = await handler({ reason: 'already there', file: path, pattern: 'done', timeout_seconds: 5000 });
    assert.match(textOf(reply), /Matched \/done\//);
    assert.match(textOf(reply), /asked for 5000 s, capped at 1800 s/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('an aborted wait stops promptly and says so', async () => {
  const { handler } = waitServerOf();
  const reply = await handler({ reason: 'doomed', seconds: 60 }, { signal: AbortSignal.abort() });
  assert.match(textOf(reply), /Wait aborted after 0 s \(doomed\)\./);

  const { dir, path } = await tempFile('quiet.log');
  try {
    const controller = new AbortController();
    setTimeout(() => controller.abort(), 30);
    const started = Date.now();
    const stopped = await handler({ reason: 'interrupted watch', file: path, pattern: 'x', timeout_seconds: 60 }, { signal: controller.signal });
    assert.ok(Date.now() - started < 1000, 'the abort is honoured mid-poll, not after the timeout');
    assert.match(textOf(stopped), /Wait aborted after 0(\.\d)? s \(interrupted watch\)\./);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('every colony is wired for waiting: server in mcpServers, prompt in the system prompt', () => {
  const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
  const server = { name: WAIT_SERVER };
  const { options } = buildOptionsWithDefaults(base, { waitServer: server });
  assert.deepEqual(options.mcpServers, { [WAIT_SERVER]: server });
  assert.ok(options.systemPrompt.append.includes(WAIT_PROMPT_APPEND));
  assert.ok(options.systemPrompt.append.includes('mcp__colonizer_wait__wait'));

  // buildOptions wires only what it is handed. The runner builds the server unconditionally, so no
  // env var or setting can leave a colony without the tool.
  const plain = buildOptionsWithDefaults(base, {}).options;
  assert.equal(plain.mcpServers, undefined);
  assert.ok(!plain.systemPrompt.append.includes(WAIT_PROMPT_APPEND));
});
