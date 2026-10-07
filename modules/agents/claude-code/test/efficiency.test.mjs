import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { AsyncQueue, buildOptions, DELEGATE_PROMPT_APPEND, environmentPrompt, packageManager, runAgent, summariseUsage } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };

/** Run `fn` on a throwaway directory carrying `files` (name -> contents), always cleaned up. */
function withRepo(files, fn) {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-pm-'));
  try {
    for (const [name, contents] of Object.entries(files)) writeFileSync(join(dir, name), contents);
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const packageJson = `${JSON.stringify({ name: 'x' })}\n`;

test('a subagent model is forced, so agents that pin their own model use it too', () => {
  const forced = buildOptions({ ...base, COLONIZER_SUBAGENT_MODEL: 'zai/glm-5.3-flash' }).options.env;
  assert.equal(forced.CLAUDE_CODE_SUBAGENT_MODEL, 'sonnet', 'a prefixed id rides an alias slot (issue #1176)');
  assert.equal(forced.ANTHROPIC_DEFAULT_SONNET_MODEL, 'zai/glm-5.3-flash');
  assert.equal(forced.CLAUDE_CODE_SUBAGENT_MODEL_FORCE, '1', 'Explore is `inherit` and would otherwise run on the orchestrator model');

  const unset = buildOptions({ ...base }).options.env;
  assert.equal(unset.CLAUDE_CODE_SUBAGENT_MODEL, undefined);
  assert.equal(unset.CLAUDE_CODE_SUBAGENT_MODEL_FORCE, undefined, 'nothing to force when no subagent model is chosen');
});

test('only Claude models are summed into cost; routed models are reported as tokens', () => {
  const usage = summariseUsage({
    'claude-opus-5': { inputTokens: 1200, outputTokens: 300, cacheReadInputTokens: 90000, cacheCreationInputTokens: 8000, costUSD: 1.5 },
    'zai/glm-5.3-flash': { inputTokens: 50000, outputTokens: 4000, cacheReadInputTokens: 0, cacheCreationInputTokens: 0, costUSD: 3.25 },
  });
  assert.equal(usage.claudeCostUsd, 1.5, 'Claude Code prices the GLM model at the Opus rate; that estimate is dropped');
  assert.deepEqual(usage.models['claude-opus-5'], { input_tokens: 1200, output_tokens: 300, cache_read_tokens: 90000, cache_write_tokens: 8000 });
  assert.deepEqual(usage.models['zai/glm-5.3-flash'], { input_tokens: 50000, output_tokens: 4000, cache_read_tokens: 0, cache_write_tokens: 0 });

  assert.equal(summariseUsage(undefined), null);
  assert.equal(summariseUsage({}), null);
  assert.deepEqual(summariseUsage({ odd: { inputTokens: 'lots' } }).models.odd, { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 });
});

test('turn_end carries per-model usage and the Claude-only cost', async () => {
  const query = ({ prompt }) => {
    const generator = (async function* () {
      for await (const _ of prompt) {
        yield {
          type: 'result',
          subtype: 'success',
          is_error: false,
          result: 'done',
          total_cost_usd: 4.75,
          duration_ms: 10,
          modelUsage: {
            'claude-opus-5': { inputTokens: 10, outputTokens: 20, cacheReadInputTokens: 30, cacheCreationInputTokens: 40, costUSD: 1.5 },
            'zai/glm-5.3-flash': { inputTokens: 1, outputTokens: 2, cacheReadInputTokens: 0, cacheCreationInputTokens: 0, costUSD: 3.25 },
          },
        };
      }
    })();
    generator.interrupt = async () => {};
    generator.close = () => {};
    return generator;
  };
  const events = [];
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', id: 'initial', text: 'go' });
  await runAgent({ query, commands, emit, graceMs: 100 });

  const [end] = events.filter((e) => e.type === 'turn_end');
  assert.equal(end.cost_usd, 1.5);
  assert.deepEqual(Object.keys(end.model_usage).sort(), ['claude-opus-5', 'zai/glm-5.3-flash']);
});

test('without per-model usage, turn_end keeps the SDK total and its old shape', async () => {
  const query = ({ prompt }) => {
    const generator = (async function* () {
      for await (const _ of prompt) yield { type: 'result', is_error: false, result: 'ok', total_cost_usd: 0.5, duration_ms: 1 };
    })();
    generator.interrupt = async () => {};
    generator.close = () => {};
    return generator;
  };
  const events = [];
  const commands = new AsyncQueue();
  commands.push({ type: 'user_message', id: 'initial', text: 'go' });
  await runAgent({ query, commands, emit: (e) => { events.push(e); if (e.type === 'turn_end') commands.push({ type: 'shutdown' }); }, graceMs: 100 });
  const [end] = events.filter((e) => e.type === 'turn_end');
  assert.equal(end.cost_usd, 0.5);
  assert.equal('model_usage' in end, false);
});

test('the colony environment is described only when the mothership names the image', () => {
  const described = buildOptions({ ...base, COLONIZER_IMAGE: 'node:24-bookworm' }).options.systemPrompt.append;
  assert.ok(described.includes(environmentPrompt('node:24-bookworm')));
  assert.match(described, /no gh CLI and no GitHub credentials/);
  assert.match(described, /`node:24-bookworm`/);
  assert.match(described, /Install a toolchain only when the task genuinely needs it/, 'installing stays possible; it is not forbidden');

  assert.ok(!buildOptions({ ...base }).options.systemPrompt.append.includes('no gh CLI'));
});

test('the package manager is detected the way done-claim verification detects it', () => {
  const pm = (files) => withRepo(files, packageManager);
  assert.deepEqual(pm({ 'package.json': packageJson, 'bun.lock': '' }), { name: 'bun', source: 'bun.lock' });
  assert.deepEqual(pm({ 'package.json': packageJson, 'bun.lockb': '' }), { name: 'bun', source: 'bun.lockb' });
  assert.deepEqual(pm({ 'package.json': packageJson, 'pnpm-lock.yaml': '' }), { name: 'pnpm', source: 'pnpm-lock.yaml' });
  assert.deepEqual(pm({ 'package.json': packageJson, 'yarn.lock': '' }), { name: 'yarn', source: 'yarn.lock' });
  assert.deepEqual(pm({ 'package.json': packageJson, 'package-lock.json': '' }), { name: 'npm', source: 'package-lock.json' });
  assert.deepEqual(pm({ 'package.json': packageJson, 'npm-shrinkwrap.json': '' }), { name: 'npm', source: 'npm-shrinkwrap.json' });
  // JS_LOCKFILES order decides it: bun wins when a bun and an npm lockfile are both there.
  assert.deepEqual(pm({ 'package.json': packageJson, 'package-lock.json': '', 'bun.lock': '' }), { name: 'bun', source: 'bun.lock' });
  // A package.json with no lockfile defaults to npm, exactly as verify.rs does.
  assert.deepEqual(pm({ 'package.json': packageJson }), { name: 'npm', source: 'package.json' });
  // No package.json at all is not a JavaScript repository, whatever lockfiles are there.
  assert.equal(pm({ 'bun.lock': '' }), null);
  assert.equal(pm({}), null);
});

test('the packageManager field settles it, and an invalid package.json falls back to the lockfiles', () => {
  const pm = (files) => withRepo(files, packageManager);
  assert.deepEqual(pm({ 'package.json': JSON.stringify({ packageManager: 'pnpm@9.1.0' }), 'package-lock.json': '' }), { name: 'pnpm', source: 'packageManager' });
  assert.deepEqual(pm({ 'package.json': JSON.stringify({ packageManager: 'bun@1.1.0' }) }), { name: 'bun', source: 'packageManager' });
  // A manager this does not know, or a bare name with no version, does not settle it.
  assert.deepEqual(pm({ 'package.json': JSON.stringify({ packageManager: 'deno@2.0.0' }) }), { name: 'npm', source: 'package.json' });
  assert.deepEqual(pm({ 'package.json': JSON.stringify({ packageManager: 'bun' }) }), { name: 'npm', source: 'package.json' });
  // Invalid JSON: the field cannot settle it, so the lockfiles do — and it never throws.
  assert.deepEqual(pm({ 'package.json': '{ not json', 'bun.lock': '' }), { name: 'bun', source: 'bun.lock' });
  assert.deepEqual(pm({ 'package.json': '{ not json' }), { name: 'npm', source: 'package.json' });
});

test('the environment prompt names the package manager when one is detected', () => {
  const named = environmentPrompt('node:24-bookworm', { name: 'bun', source: 'bun.lock' });
  assert.match(named, /- This repository uses bun \(from `bun\.lock`\): install and test with `bun` rather than another package manager; done-claim verification runs it the same way\./);
  assert.ok(named.includes(environmentPrompt('node:24-bookworm')), 'the image bullets stay a prefix of the named form');

  assert.doesNotMatch(environmentPrompt('node:24-bookworm', null), /package manager/);
  assert.doesNotMatch(environmentPrompt('node:24-bookworm'), /package manager/, 'absent when nothing is detected');
});

test('the orchestrator asks for focused reports and waits instead of idling', () => {
  assert.match(DELEGATE_PROMPT_APPEND, /file:line references/);
  assert.match(DELEGATE_PROMPT_APPEND, /Do not ask for exhaustive, verbatim or "in full" dumps/);
  assert.match(DELEGATE_PROMPT_APPEND, /mcp__colonizer_wait__wait holds the turn/);
  assert.match(DELEGATE_PROMPT_APPEND, /never a placeholder command such as sleep or echo/);
});
