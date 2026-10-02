import assert from 'node:assert/strict';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { backgroundRecordName, buildOptions } from '../runner.mjs';

const servicesDir = () => mkdtemp(join(tmpdir(), 'colonizer-svc-'));

// The recording hook is installed only when the mothership mounted the services dir.
const postToolUseHook = (env) => {
  const { options } = buildOptions(env);
  return options.hooks.PostToolUse[0].hooks[0];
};

test('a background Bash call leaves a restart:false record that carries no env values', async () => {
  const dir = await servicesDir();
  const hook = postToolUseHook({ COLONIZER_SERVICES_DIR: dir, COLONIZER_SECRET_TOKEN: 'sk-ant-do-not-leak' });
  await hook({ tool_name: 'Bash', tool_input: { command: 'cargo test --workspace', run_in_background: true }, tool_use_id: 'toolu_01' });
  const files = await readdir(dir);
  assert.deepEqual(files, [`${backgroundRecordName('cargo test --workspace')}.json`]);
  const record = JSON.parse(await readFile(join(dir, files[0]), 'utf8'));
  assert.deepEqual(record, { name: backgroundRecordName('cargo test --workspace'), cmd: 'cargo test --workspace', restart: false, source: 'background' });
  assert.ok(!JSON.stringify(record).includes('sk-ant-do-not-leak'), 'no env value may travel into a record');
  await rm(dir, { recursive: true, force: true });
});

test('foreground calls record nothing, and without the services dir there is no hook at all', async () => {
  const dir = await servicesDir();
  const hook = postToolUseHook({ COLONIZER_SERVICES_DIR: dir });
  await hook({ tool_name: 'Bash', tool_input: { command: 'ls', run_in_background: false }, tool_use_id: 'toolu_02' });
  await hook({ tool_name: 'Bash', tool_input: { command: '   ' }, tool_use_id: 'toolu_03' });
  assert.deepEqual(await readdir(dir), []);
  assert.equal(buildOptions({}).options.hooks?.PostToolUse, undefined);
  await rm(dir, { recursive: true, force: true });
});

test('a failure to record never blocks the tool call', async () => {
  const hook = postToolUseHook({ COLONIZER_SERVICES_DIR: join(tmpdir(), 'colonizer-svc-nonexistent') });
  const result = await hook({ tool_name: 'Bash', tool_input: { command: 'cargo build', run_in_background: true } });
  assert.deepEqual(result, { continue: true });
});

test('the record name hashes the command, so reruns overwrite instead of accumulating', () => {
  assert.match(backgroundRecordName('cargo test'), /^bg-[0-9a-f]{8}$/);
  assert.equal(backgroundRecordName('cargo test'), backgroundRecordName('cargo test'));
  assert.notEqual(backgroundRecordName('a'), backgroundRecordName('b'));
});
