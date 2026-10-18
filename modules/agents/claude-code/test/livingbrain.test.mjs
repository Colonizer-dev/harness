import assert from 'node:assert/strict';
import { test } from 'node:test';

import { LIVINGBRAIN_PROMPT_APPEND, LIVINGBRAIN_SERVER, livingBrainServer } from '../livingbrain.mjs';
import { buildOptions } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
const granted = {
  ...base,
  COLONIZER_LIVINGBRAIN_MCP_URL: 'https://mcp.livingbrain.wiki/v1/pages/mcp',
  COLONIZER_LIVINGBRAIN_TOKEN: 'msb-placeholder-1234',
};

test('no grant, no server: both variables are needed and the URL must be https', () => {
  assert.equal(livingBrainServer({}), null);
  assert.equal(livingBrainServer({ COLONIZER_LIVINGBRAIN_MCP_URL: granted.COLONIZER_LIVINGBRAIN_MCP_URL }), null);
  assert.equal(livingBrainServer({ ...granted, COLONIZER_LIVINGBRAIN_MCP_URL: 'http://mcp.example/v1' }), null);
});

test('a granted colony mounts the brain as a remote http server carrying the placeholder bearer', () => {
  const { options } = buildOptions(granted);
  assert.deepEqual(options.mcpServers[LIVINGBRAIN_SERVER], {
    type: 'http',
    url: 'https://mcp.livingbrain.wiki/v1/pages/mcp',
    headers: { Authorization: 'Bearer msb-placeholder-1234' },
  });
  assert.ok(options.systemPrompt.append.includes(LIVINGBRAIN_PROMPT_APPEND));
  // Claude Code's own environment (and so every Bash call) never sees the variables.
  assert.equal(options.env.COLONIZER_LIVINGBRAIN_TOKEN, undefined);
  assert.equal(options.env.COLONIZER_LIVINGBRAIN_MCP_URL, undefined);
});

test('an ungranted colony has neither the server nor the prompt block', () => {
  const { options } = buildOptions({ ...base });
  assert.equal(options.mcpServers?.[LIVINGBRAIN_SERVER], undefined);
  assert.ok(!options.systemPrompt.append.includes('<living-brain>'));
});
