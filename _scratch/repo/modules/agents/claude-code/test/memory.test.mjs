import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { z } from 'zod';

import {
  createMemoryServer,
  formatResults,
  MEMORY_PROMPT_APPEND,
  MEMORY_PROPOSE_TOOL,
  MEMORY_SERVER,
  MEMORY_TOOLS,
  memoryDecision,
  PROPOSED_REPLY,
  searchMemory,
} from '../memory.mjs';
import { buildOptions as buildOptionsWithDefaults, SYSTEM_PROMPT_APPEND } from '../runner.mjs';

// These tests cover other features. Delegation is enforced by default and has its own tests in
// delegate.test.mjs, so it is switched off here unless a test asks for it.
const buildOptions = (env = {}, extra) => buildOptionsWithDefaults({ COLONIZER_DELEGATE: 'off', ...env }, extra);

async function memoryDir() {
  const dir = await mkdtemp(join(tmpdir(), 'colonizer-memory-'));
  const note = async (scope, file, text) => {
    await mkdir(join(dir, scope, 'notes'), { recursive: true });
    await writeFile(join(dir, scope, 'notes', file), text);
  };
  await note('repo', 'tests.md', '# Run tests with --locked\n\nAlways run `cargo test --locked`; CI rejects lockfile drift.');
  await note('org', 'style.md', '# Commit style\n\nImperative subject lines, no ticket prefixes.');
  await note('global', 'tests-global.md', 'Prefer running the TESTS of the package you changed, not the whole workspace.');
  return dir;
}

test('memory_search matches all terms case-insensitively across scopes', async () => {
  const dir = await memoryDir();
  try {
    const locked = await searchMemory(dir, 'Tests LOCKED');
    assert.equal(locked.length, 1);
    assert.equal(locked[0].scope, 'repo');
    assert.equal(locked[0].title, 'Run tests with --locked');
    assert.equal(locked[0].file, 'repo/notes/tests.md');
    assert.match(locked[0].snippet, /cargo test --locked/);

    const tests = await searchMemory(dir, 'tests');
    assert.deepEqual(tests.map((r) => r.scope), ['repo', 'global']);
    assert.equal(tests[1].title, 'Prefer running the TESTS of the package you changed, not the whole workspace.');

    assert.deepEqual(await searchMemory(dir, 'tests nonexistent'), []);
    assert.deepEqual(await searchMemory(dir, '   '), []);
    assert.equal((await searchMemory(dir, 'e', { limit: 2 })).length, 2);
    assert.equal(formatResults([]), 'No shared memory matches that query.');
    assert.deepEqual(await searchMemory(join(dir, 'missing'), 'tests'), []);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('memory tools: search returns text, propose emits a proposal event', async () => {
  const dir = await memoryDir();
  const events = [];
  const defs = [];
  const fakeTool = (name, description, shape, handler) => {
    const def = { name, description, shape, handler };
    defs.push(def);
    return def;
  };
  let serverOptions;
  const fakeCreate = (options) => {
    serverOptions = options;
    return { type: 'sdk', name: options.name, instance: {} };
  };
  try {
    const server = createMemoryServer({ dir, emit: (e) => events.push(e), createSdkMcpServer: fakeCreate, tool: fakeTool, z });
    assert.equal(server.name, MEMORY_SERVER);
    assert.deepEqual(serverOptions.tools.map((t) => t.name), ['memory_search', 'memory_propose']);
    assert.deepEqual(MEMORY_TOOLS, ['mcp__colonizer_memory__memory_search', 'mcp__colonizer_memory__memory_propose']);

    const [search, propose] = defs;
    const found = await search.handler({ query: 'commit style' });
    assert.match(found.content[0].text, /\[org\] Commit style/);

    const result = await propose.handler({ scope: 'repo', title: 'Use --locked', content: 'Run cargo with --locked.' });
    assert.deepEqual(result, { content: [{ type: 'text', text: PROPOSED_REPLY }] });
    assert.deepEqual(events, [{ type: 'memory_proposal', scope: 'repo', title: 'Use --locked', content: 'Run cargo with --locked.', tags: [] }]);

    // The input schemas enforce the protocol limits.
    const proposeSchema = z.object(propose.shape);
    assert.equal(proposeSchema.safeParse({ scope: 'team', title: 't', content: 'c' }).success, false);
    assert.equal(proposeSchema.safeParse({ scope: 'org', title: 'x'.repeat(201), content: 'c' }).success, false);
    assert.equal(proposeSchema.safeParse({ scope: 'global', title: 't', content: 'c', tags: ['a'] }).success, true);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('buildOptions maps routing and memory settings into Claude Code options', () => {
  const memoryServer = { type: 'sdk', name: MEMORY_SERVER, instance: {} };
  const env = {
    PATH: '/usr/bin',
    COLONIZER_MODEL: 'opus',
    COLONIZER_SUBAGENT_MODEL: 'deepseek/deepseek-flash',
    COLONIZER_BACKGROUND_MODEL: 'local/qwen-small',
    COLONIZER_PROVIDER_KEY_DEEPSEEK: 'placeholder',
    COLONIZER_MEMORY_DIR: '/colonizer/memory',
    CLAUDE_CODE_SUBAGENT_MODEL: 'from-outer-env',
  };
  const { options } = buildOptions(env, {
    routerUrl: 'http://127.0.0.1:4545',
    memoryServer,
    hiddenEnv: ['COLONIZER_PROVIDER_KEY_DEEPSEEK'],
    routes: [{ prefix: 'deepseek/', timeout_secs: 900, context_tokens: 131072 }],
  });
  assert.equal(options.env.CLAUDE_STREAM_IDLE_TIMEOUT_MS, '900000');
  assert.equal(options.env.CLAUDE_CODE_MAX_CONTEXT_TOKENS, '131072');
  assert.equal(options.model, 'opus');
  assert.equal(options.env.ANTHROPIC_BASE_URL, 'http://127.0.0.1:4545');
  assert.equal(options.env.CLAUDE_CODE_SUBAGENT_MODEL, 'deepseek/deepseek-flash');
  assert.equal(options.env.ANTHROPIC_DEFAULT_HAIKU_MODEL, 'local/qwen-small');
  assert.equal(options.env.COLONIZER_PROVIDER_KEY_DEEPSEEK, undefined);
  assert.deepEqual(options.mcpServers, { [MEMORY_SERVER]: memoryServer });
  // Memory tools are allowed by canUseTool; listing them in allowedTools would shadow it.
  assert.equal(options.allowedTools, undefined);
  assert.equal(options.systemPrompt.append, `${SYSTEM_PROMPT_APPEND}\n${MEMORY_PROMPT_APPEND}`);

  const plain = buildOptions({ PATH: '/usr/bin' }).options;
  assert.equal(plain.env.ANTHROPIC_BASE_URL, undefined);
  assert.equal(plain.env.CLAUDE_CODE_SUBAGENT_MODEL, undefined);
  assert.equal(plain.mcpServers, undefined);
  assert.equal(plain.allowedTools, undefined);
  assert.equal(plain.systemPrompt.append, SYSTEM_PROMPT_APPEND);

  // Memory needs both the mounted directory and a server.
  assert.equal(buildOptions({ COLONIZER_MEMORY_DIR: '/colonizer/memory' }).options.mcpServers, undefined);
});

test('only the orchestrator proposes: a subagent is told to report instead', () => {
  const [search] = MEMORY_TOOLS;
  assert.equal(memoryDecision(MEMORY_PROPOSE_TOOL, {}), null, 'the orchestrator may propose');
  assert.match(memoryDecision(MEMORY_PROPOSE_TOOL, { agent_id: 'agent_01' }), /Only the orchestrator proposes shared memory/);
  assert.equal(memoryDecision(search, { agent_id: 'agent_01' }), null, 'a subagent still searches');
  assert.equal(memoryDecision('Bash', { agent_id: 'agent_01' }), null, 'other tools are not this gate’s business');
});

test('the memory gate is registered only with memory, and refuses a subagent even when delegation is off', async () => {
  const memoryServer = { type: 'sdk', name: MEMORY_SERVER, instance: {} };
  assert.equal(buildOptions({}, { memoryServer }).options.hooks, undefined, 'no memory, no gate');

  const { options } = buildOptions({ COLONIZER_MEMORY_DIR: '/colonizer/memory' }, { memoryServer });
  const hooks = options.hooks.PreToolUse.flatMap((entry) => entry.hooks);
  const decide = async (input) => {
    for (const hook of hooks) {
      const out = await hook({ hook_event_name: 'PreToolUse', tool_input: {}, ...input });
      if (out.hookSpecificOutput?.permissionDecision === 'deny') return out.hookSpecificOutput;
    }
    return null;
  };
  assert.equal(await decide({ tool_name: MEMORY_PROPOSE_TOOL }), null);
  assert.equal(await decide({ tool_name: MEMORY_TOOLS[0], agent_id: 'agent_01' }), null);
  const denied = await decide({ tool_name: MEMORY_PROPOSE_TOOL, agent_id: 'agent_01' });
  assert.match(denied.permissionDecisionReason, /Only the orchestrator proposes shared memory/);
});
