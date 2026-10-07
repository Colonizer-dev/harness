import assert from 'node:assert/strict';
import { appendFileSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { AsyncQueue, buildOptions, jevVersionOk, parseJevDecisions, runAgent } from '../runner.mjs';

const fixed = (...messages) =>
  async function* () {
    for (const m of messages) yield m;
  };

/** A fake Agent SDK `query` that plays `turnFn`, with the protocol drip needed to shut down. */
async function run(turnFn, options) {
  const query = () => {
    const generator = turnFn();
    generator.interrupt = async () => {};
    generator.close = () => {};
    return generator;
  };
  const events = [];
  const commands = new AsyncQueue();
  commands.push({ type: 'user_message', text: 'go' });
  const emit = (e) => {
    events.push(e);
    if (e.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  await runAgent({ query, commands, emit, options, graceMs: 100 });
  return events;
}

const jevOptions = (extra = {}) =>
  buildOptions({ COLONIZER_DELEGATE: 'off', COLONIZER_JEV_COMPACTION: 'true', ...extra }).options;
const okInit = {
  type: 'system',
  subtype: 'init',
  session_id: 's1',
  model: 'm',
  claude_code_version: '2.1.280 (Claude Code)',
  plugins: [{ name: 'fast-jev-compaction', path: '/opt/colonizer/jev-compaction' }],
};
const done = { type: 'result', subtype: 'success', is_error: false, result: 'ok', total_cost_usd: 0, duration_ms: 1 };
const logs = (events, level) => events.filter((e) => e.type === 'log' && (!level || e.level === level));

test('jev compaction is off by default, and a stale function-hooks flag is dropped', () => {
  const { options } = buildOptions({ COLONIZER_DELEGATE: 'off' });
  assert.equal(options.plugins, undefined);
  assert.equal(options.settings, undefined);
  assert.equal(options.debugFile, undefined);
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, undefined);

  const stale = buildOptions({ COLONIZER_DELEGATE: 'off', CLAUDE_CODE_ENABLE_FUNCTION_HOOKS: '1' });
  assert.equal(stale.options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, undefined);
});

test('jev compaction loads the plugin, sets the flag, and parses its numbers', () => {
  const options = jevOptions({ COLONIZER_JEV_KEEP_THRESHOLD: '0.7', COLONIZER_JEV_PRESERVE_RECENT: '10' });
  assert.deepEqual(options.plugins, [{ type: 'local', path: '/opt/colonizer/jev-compaction' }]);
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, '1');
  assert.deepEqual(options.settings.pluginConfigs['fast-jev-compaction@inline'].options, {
    keepThreshold: 0.7,
    preserveRecentMessages: 10,
  });
  assert.ok(options.debugFile.startsWith(tmpdir()));

  const bad = jevOptions({ COLONIZER_JEV_KEEP_THRESHOLD: 'high' });
  assert.deepEqual(bad.settings.pluginConfigs['fast-jev-compaction@inline'].options, {});
  const custom = jevOptions({ COLONIZER_JEV_COMPACTION_DIR: '/mnt/jev' });
  assert.deepEqual(custom.plugins, [{ type: 'local', path: '/mnt/jev' }]);
});

test('jev compaction composes with rtk', () => {
  const options = jevOptions({ COLONIZER_RTK: 'true' });
  assert.ok(options.env.PATH.startsWith('/opt/colonizer/bin:'), options.env.PATH);
  assert.ok(options.hooks.PreToolUse.some((e) => e.matcher === 'Bash'), 'the rtk Bash hook is still there');
  assert.deepEqual(options.plugins, [{ type: 'local', path: '/opt/colonizer/jev-compaction' }]);
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, '1');
});

test('jev compaction leaves the Headroom router URL alone', () => {
  const { options } = buildOptions({ COLONIZER_DELEGATE: 'off', COLONIZER_JEV_COMPACTION: 'true' }, { routerUrl: 'http://127.0.0.1:9999' });
  assert.equal(options.env.ANTHROPIC_BASE_URL, 'http://127.0.0.1:9999');
  assert.ok(options.plugins.some((p) => p.path === '/opt/colonizer/jev-compaction'));
  assert.equal(options.env.CLAUDE_CODE_ENABLE_FUNCTION_HOOKS, '1');
});

test('the version check tolerates suffixes and patch versions', () => {
  assert.ok(jevVersionOk('2.1.274'));
  assert.ok(jevVersionOk('2.1.280 (Claude Code)'));
  assert.ok(jevVersionOk('2.2'));
  assert.ok(!jevVersionOk('2.1.100'));
  assert.ok(!jevVersionOk(undefined));
});

test('init warns on an old Claude Code and on a missing plugin', async () => {
  const options = jevOptions();
  const old = await run(fixed({ ...okInit, claude_code_version: '2.1.100', plugins: [] }, done), options);
  assert.match(logs(old, 'warn')[0].message, /needs Claude Code 2\.1\.274 or later.*2\.1\.100.*built-in summary/);

  const missing = await run(fixed({ ...okInit, plugins: [{ name: 'other', path: '/x' }] }, done), options);
  assert.match(logs(missing, 'warn')[0].message, /didn't load the fast-jev-compaction plugin/);

  const ok = await run(fixed(okInit, done), options);
  assert.deepEqual(logs(ok, 'warn'), []);
});

test('a verdict written before init is ignored; one written after is reported', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    writeFileSync(log, '[debug] kept 1/10 messages, no summary (stale)\n');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const compact = (compact_metadata) => ({ type: 'system', subtype: 'compact_boundary', compact_metadata });
    const events = await run(async function* () {
      yield okInit;
      yield compact({ trigger: 'auto', pre_tokens: 1, post_tokens: 2 });
      appendFileSync(log, '[debug] kept 120/300 messages, no summary (fresh)\n');
      yield compact({ trigger: 'auto', pre_tokens: 3, post_tokens: 4 });
      yield done;
    }, options);
    const compactions = logs(events, 'info').map((e) => e.message).filter((m) => m.startsWith('Compaction'));
    assert.equal(compactions.length, 2);
    assert.match(compactions[0], /left no verdict/);
    assert.ok(!compactions[0].includes('stale'), 'the pre-init verdict is not re-reported');
    assert.match(compactions[1], /kept 120\/300 messages, no summary/);
    assert.ok(!compactions[1].includes('stale'), 'only the new bytes are read');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('compact_boundary reports the plugin verdict from the debug log', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const compact = (compact_metadata) => ({ type: 'system', subtype: 'compact_boundary', compact_metadata });
    const events = await run(async function* () {
      yield okInit;
      writeFileSync(log, 'noise\n[debug] $.ui.toast: kept 120/300 messages, no summary (score 0.42)\n');
      yield compact({ trigger: 'auto', pre_tokens: 90000, post_tokens: 12000 });
      rmSync(log); // a rotated-away log reads as no verdict, never an error
      yield compact({ trigger: 'manual', pre_tokens: 50000 });
      writeFileSync(log, 'fallback to built-in summary (hook timed out)\n');
      yield compact({ trigger: 'auto', pre_tokens: 80000, post_tokens: 80000 });
      yield done;
    }, options);
    const compactions = logs(events, 'info').map((e) => e.message).filter((m) => m.startsWith('Compaction'));
    assert.equal(compactions.length, 3);
    assert.match(compactions[0], /Compaction \(auto, 90000 → 12000 tokens\): kept 120\/300 messages, no summary/);
    assert.match(compactions[1], /Compaction \(manual, 50000 → \? tokens\): fast-jev-compaction left no verdict/);
    assert.match(compactions[2], /fallback to built-in summary/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('with jev off, compact_boundary stays ignored', async () => {
  const { options } = buildOptions({ COLONIZER_DELEGATE: 'off' });
  const compact = { type: 'system', subtype: 'compact_boundary', compact_metadata: { trigger: 'auto', pre_tokens: 1, post_tokens: 2 } };
  const events = await run(fixed(okInit, compact, done), options);
  assert.ok(!logs(events).some((e) => e.message.startsWith('Compaction')));
});

// The visibility ladder (issue #475): Jev scores each tool result's relevance in its decisions log,
// and the runner maps those scores back onto the tool calls they were about.

const toolUse = (messageId, calls) => ({
  type: 'assistant',
  message: { id: messageId, content: calls.map(([tool_call_id, name]) => ({ type: 'tool_use', id: tool_call_id, name, input: {} })) },
});
const toolResults = (results) => ({
  type: 'user',
  message: { content: results.map(([tool_call_id, output]) => ({ type: 'tool_result', tool_use_id: tool_call_id, content: output })) },
});
const compactBoundary = (compact_metadata) => ({ type: 'system', subtype: 'compact_boundary', compact_metadata });
const ladders = (events) => events.filter((e) => e.type === 'jev_ladder');
const appendJevLog = (log, lines) => appendFileSync(log, `${lines.join('\n')}\n`);

test('parseJevDecisions reads the last pass decisions, plain or chunked', () => {
  assert.deepEqual(parseJevDecisions('noise\ndecisions: t1:Bash:keep/call=0.98/result=0.87 t2:Read:drop_call/call=0.12/result=0.05\n'), [
    { n: 1, tool: 'Bash', action: 'keep', keepCall: 0.98, keepResult: 0.87 },
    { n: 2, tool: 'Read', action: 'drop_call', keepCall: 0.12, keepResult: 0.05 },
  ]);
  assert.deepEqual(parseJevDecisions('[debug] no decisions here\nkept 8/12 messages, no summary\n'), []);
  assert.deepEqual(parseJevDecisions('decisions: \n'), []);
  // One pass per read window, so an earlier pass's group is superseded; long groups chunk across
  // `decisions (i/n): ` lines that read as one.
  assert.deepEqual(
    parseJevDecisions(
      [
        'decisions: t1:Bash:drop_call/call=0.1/result=0.1',
        'kept 8/12 messages, no summary',
        'decisions (1/2): t1:Bash:keep/call=0.98/result=0.87',
        'decisions (2/2): t2:Read:drop_call/call=0.12/result=0.05',
        'kept 9/12 messages, no summary',
      ].join('\n'),
    ),
    [
      { n: 1, tool: 'Bash', action: 'keep', keepCall: 0.98, keepResult: 0.87 },
      { n: 2, tool: 'Read', action: 'drop_call', keepCall: 0.12, keepResult: 0.05 },
    ],
  );
});

test('compact_boundary maps decisions onto the calls they were about', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const events = await run(async function* () {
      yield okInit;
      yield toolUse('m1', [
        ['call-1', 'Bash'],
        ['call-2', 'Read'],
      ]);
      yield toolResults([
        ['call-1', 'out one'],
        ['call-2', 'out two'],
      ]);
      appendJevLog(log, [
        'decisions: t1:Bash:keep/call=0.98/result=0.87 t2:Read:drop_call/call=0.12/result=0.05',
        'kept 8/12 messages, no summary',
      ]);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 90000, post_tokens: 12000 });
      yield done;
    }, options);
    assert.deepEqual(ladders(events), [
      {
        type: 'jev_ladder',
        applied: true,
        pre_tokens: 90000,
        post_tokens: 12000,
        trigger: 'auto',
        decisions: [
          { tool_call_id: 'call-1', tool: 'Bash', action: 'keep', keep_call: 0.98, keep_result: 0.87 },
          { tool_call_id: 'call-2', tool: 'Read', action: 'drop_call', keep_call: 0.12, keep_result: 0.05 },
        ],
      },
    ]);
    // Additive: the plain compaction log is unchanged alongside the ladder event.
    assert.match(logs(events, 'info').map((e) => e.message).find((m) => m.startsWith('Compaction')), /kept 8\/12 messages/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('after an applied drop_call, later decisions renumber onto the surviving pairs', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const events = await run(async function* () {
      yield okInit;
      yield toolUse('m1', [
        ['call-1', 'Bash'],
        ['call-2', 'Read'],
      ]);
      yield toolResults([
        ['call-1', 'out one'],
        ['call-2', 'out two'],
      ]);
      appendJevLog(log, [
        'decisions: t1:Bash:keep/call=0.9/result=0.9 t2:Read:drop_call/call=0.1/result=0.1',
        'kept 8/12 messages, no summary',
      ]);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 100, post_tokens: 50 });
      // Read (pair 2) is gone from the transcript, so Grep becomes pair 2 of the next pass.
      yield toolUse('m2', [['call-3', 'Grep']]);
      yield toolResults([['call-3', 'out three']]);
      appendJevLog(log, [
        'decisions: t1:Bash:drop_result/call=0.7/result=0.6 t2:Grep:keep/call=0.8/result=0.9',
        'kept 6/10 messages, no summary',
      ]);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 90, post_tokens: 60 });
      yield done;
    }, options);
    const [first, second] = ladders(events);
    assert.deepEqual(first.decisions.map((d) => [d.tool_call_id, d.action]), [
      ['call-1', 'keep'],
      ['call-2', 'drop_call'],
    ]);
    assert.deepEqual(second.decisions.map((d) => [d.tool_call_id, d.tool]), [
      ['call-1', 'Bash'],
      ['call-3', 'Grep'],
    ]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a fallback pass emits shadow decisions and removes nothing', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const events = await run(async function* () {
      yield okInit;
      yield toolUse('m1', [
        ['call-1', 'Bash'],
        ['call-2', 'Read'],
      ]);
      yield toolResults([
        ['call-1', 'out one'],
        ['call-2', 'out two'],
      ]);
      appendJevLog(log, [
        'decisions: t1:Bash:keep/call=0.9/result=0.9 t2:Read:drop_call/call=0.1/result=0.1',
        'fallback to built-in summary (minReductionRatio not met)',
      ]);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 100, post_tokens: 100 });
      yield toolUse('m2', [['call-3', 'Grep']]);
      yield toolResults([['call-3', 'out three']]);
      // Nothing left the transcript, so Read is still pair 2 of the next pass.
      appendJevLog(log, ['decisions: t2:Read:drop_call/call=0.1/result=0.1', 'kept 6/10 messages, no summary']);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 90, post_tokens: 40 });
      yield done;
    }, options);
    const [fallback, applied] = ladders(events);
    assert.equal(fallback.applied, false);
    assert.deepEqual(fallback.decisions.map((d) => d.tool_call_id), ['call-1', 'call-2']);
    assert.equal(applied.applied, true);
    assert.deepEqual(applied.decisions.map((d) => d.tool_call_id), ['call-2']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('decisions without a verdict line are not applied', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const events = await run(async function* () {
      yield okInit;
      yield toolUse('m1', [['call-1', 'Bash']]);
      yield toolResults([['call-1', 'out one']]);
      // Decisions made it into the window, but no verdict line did: the pass's outcome is
      // unconfirmed, so nothing may leave the live pairs.
      appendJevLog(log, ['decisions: t1:Bash:drop_call/call=0.1/result=0.1']);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 100, post_tokens: 50 });
      yield toolUse('m2', [['call-2', 'Grep']]);
      yield toolResults([['call-2', 'out two']]);
      appendJevLog(log, ['decisions: t1:Bash:keep/call=0.9/result=0.9 t2:Grep:keep/call=0.8/result=0.8', 'kept 6/10 messages, no summary']);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 90, post_tokens: 60 });
      yield done;
    }, options);
    const [unconfirmed, applied] = ladders(events);
    assert.equal(unconfirmed.applied, false);
    assert.deepEqual(unconfirmed.decisions.map((d) => d.tool_call_id), ['call-1']);
    // Bash never actually left the transcript, so the next pass still sees it as pair 1.
    assert.deepEqual(applied.decisions.map((d) => d.tool_call_id), ['call-1', 'call-2']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('decisions with gaps for pinned pairs resolve by actual position', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-jev-'));
  try {
    const log = join(dir, 'debug.log');
    const options = jevOptions({ COLONIZER_JEV_COMPACTION_LOG: log });
    const events = await run(async function* () {
      yield okInit;
      yield toolUse('m1', [
        ['call-1', 'Bash'],
        ['call-2', 'Read'],
        ['call-3', 'Grep'],
        ['call-4', 'WebFetch'],
      ]);
      yield toolResults([
        ['call-1', 'out one'],
        ['call-2', 'out two'],
        ['call-3', 'out three'],
        ['call-4', 'out four'],
      ]);
      // The recent pairs (t2, t3) are pinned and never get a token.
      appendJevLog(log, [
        'decisions: t1:Bash:keep/call=0.9/result=0.9 t4:WebFetch:drop_result/call=0.2/result=0.3',
        'kept 10/14 messages, no summary',
      ]);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 120, post_tokens: 80 });
      // t7 names no pair this session tracked; it is skipped, not an error.
      appendJevLog(log, ['decisions: t1:Bash:drop_call/call=0.4/result=0.4 t7:Edit:keep/call=0.5/result=0.5', 'kept 8/12 messages, no summary']);
      yield compactBoundary({ trigger: 'auto', pre_tokens: 80, post_tokens: 40 });
      yield done;
    }, options);
    const [first, second] = ladders(events);
    assert.deepEqual(first.decisions, [
      { tool_call_id: 'call-1', tool: 'Bash', action: 'keep', keep_call: 0.9, keep_result: 0.9 },
      { tool_call_id: 'call-4', tool: 'WebFetch', action: 'drop_result', keep_call: 0.2, keep_result: 0.3 },
    ]);
    assert.deepEqual(second.decisions, [{ tool_call_id: 'call-1', tool: 'Bash', action: 'drop_call', keep_call: 0.4, keep_result: 0.4 }]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
