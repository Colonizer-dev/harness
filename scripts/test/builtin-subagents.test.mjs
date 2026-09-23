import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { compare, extractBuiltins, LOCK, lockRows, markdown, ours, readBunModules, REFRESH, SNAPSHOT } from '../builtin-subagents.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const snapshot = JSON.parse(readFileSync(SNAPSHOT, 'utf8'));

test('the built-in snapshot is of the Claude Code build the lock pins', () => {
  const rows = lockRows(LOCK).filter((row) => row.kind === 'agent');
  assert.ok(rows.length, 'vendor/claude-code.lock pins no Claude Code build');
  for (const row of rows) {
    assert.equal(
      snapshot.version,
      row.version,
      `vendor/claude-code.lock pins ${row.version} (${row.platform}) but vendor/claude-code-builtins.json is from ${snapshot.version}: ` +
        `re-extract it with \`${REFRESH}\` and bring modules/agents/claude-code/subagents.mjs in step`,
    );
  }
});

test('subagents.mjs copies the built-ins verbatim and denies at least what they deny', () => {
  const stale = compare(snapshot, ours());
  assert.deepEqual(stale, [], markdown(snapshot.version, stale));
});

test('a built-in that denies more, or reads differently, is stale and shows the diff', () => {
  const changed = structuredClone(snapshot);
  changed.Explore.disallowedTools.push('Bash');
  changed.Explore.prompt[0] = 'You are a search agent.';
  const differences = compare(changed, snapshot);
  assert.deepEqual(differences.map((d) => [d.agent, d.missing ?? d.field]), [
    ['Explore', ['Bash']],
    ['Explore', 'prompt'],
  ]);
  const section = markdown('9.9.9', differences, [['ea', 'function ea(){return!0}']]);
  assert.match(section, /The built-in `Explore` denies `Bash`, and ours does not/);
  assert.match(section, /commits pushed to it are lost/);
  assert.match(section, /^-You are a file search specialist/m);
  assert.match(section, /^\+You are a search agent\.$/m);
  assert.match(section, /^ea: function ea\(\)\{return!0\}$/m);
});

test('a file the lock does not pin is refused', () => {
  const run = spawnSync(process.execPath, [join(ROOT, 'scripts/builtin-subagents.mjs'), fileURLToPath(import.meta.url)], { encoding: 'utf8' });
  assert.equal(run.status, 1);
  assert.match(run.stderr, /builtin-subagents\.test\.mjs \(sha256 [0-9a-f]{64}\) is not a build vendor\/claude-code\.lock pins/);
});

// Two chunks shaped like 2.1.280's: the agent objects import their tool names (one renamed on each
// side), a deny list spreads a constant defined before what it names, the Explore prompt asks a helper
// about the environment and has an escape, a decoy object names general-purpose without a prompt, and
// another chunk defines ze too.
const TOOLS = 'var ag="Agent";var wr=[an,"ArtifactComments"],an="Artifact",ze="Bash";export{ag as mt,wr as C7,ze};';
const AGENTS = [
  'import{mt as Xa,C7,ze}from"/$bunfs/root/chunk-tools.js";',
  'function ea(){return process.platform!=="win32"}',
  'function KJt(){let e=ea();return`Search \\u2014 read-only.\\n- Use ${e?ze:"PowerShell"} ONLY to read`}',
  'var VJt=\'Fast read-only search agent\',DS={agentType:"Explore",whenToUse:VJt,disallowedTools:[Xa,...C7,"Edit"],getSystemPrompt:()=>KJt()};',
  'var dflt={agentType:"general-purpose",model:"inherit"};',
  'function o4n(){return"You are an agent"}var oq={agentType:"general-purpose",whenToUse:"General-purpose agent",tools:["*"],getSystemPrompt:o4n};',
].join('\n');
const modules = (agents = AGENTS) =>
  new Map([
    ['/$bunfs/root/chunk-tools.js', TOOLS],
    ['/$bunfs/root/chunk-agents.js', agents],
    ['/$bunfs/root/chunk-other.js', 'var ze="PowerShell";export{ze};'],
  ]);
const EXPECTED = {
  'general-purpose': { description: 'General-purpose agent', prompt: ['You are an agent'] },
  Explore: {
    description: 'Fast read-only search agent',
    prompt: ['Search — read-only.', '- Use Bash ONLY to read'],
    disallowedTools: ['Agent', 'Artifact', 'ArtifactComments', 'Edit'],
  },
};

test('the built-ins are extracted through imports, spreads and stubbed helpers', () => {
  const stubbed = [];
  assert.deepEqual(extractBuiltins(modules(), stubbed), EXPECTED);
  assert.deepEqual(stubbed, [['ea', 'function ea(){return process.platform!=="win32"}']]);
});

test('what cannot be resolved fails the extraction, naming it', () => {
  const broken = (from, to) => () => extractBuiltins(modules(AGENTS.replace(from, to)));
  assert.throws(broken('whenToUse:VJt', 'whenToUse:Nope'), /Nope: no definition and no import in \/\$bunfs\/root\/chunk-agents\.js/);
  assert.throws(broken('var dflt', 'function x(){function KJt(){}}var dflt'), /KJt: 2 candidate definitions/);
  assert.throws(broken('${e?ze:"PowerShell"}', '${ea()}'), /ea\(\) is stubbed as a condition/);
  assert.throws(broken('var VJt=\'Fast read-only search agent\'', 'var VJt=\'Fast\'+x'), /VJt .* is more than a literal/);
  assert.throws(broken('getSystemPrompt:o4n', 'prompt:o4n'), /0 built-in general-purpose agents/);
});

test('a Bun standalone executable gives up its modules', () => {
  // The payload sits between the rest of the executable and a 32-byte header, then the trailer, then
  // (as in the ELF builds) more of the executable.
  const files = [...modules(), ['/$bunfs/root/README.md', 'UTF-8 — here']];
  const parts = [];
  let at = 0;
  const put = (text) => (parts.push(Buffer.from(text)), (at += Buffer.byteLength(text)), [at - Buffer.byteLength(text), Buffer.byteLength(text)]);
  const records = files.map(([name, contents]) => {
    const record = Buffer.alloc(52);
    [...put(name), ...put(contents)].forEach((value, i) => record.writeUInt32LE(value, 4 * i));
    record[48] = name.endsWith('.md') ? 2 : 1;
    return record;
  });
  const payload = Buffer.concat([...parts, ...records]);
  const header = Buffer.alloc(32);
  header.writeBigUInt64LE(BigInt(payload.length));
  header.writeUInt32LE(at, 8);
  header.writeUInt32LE(52 * records.length, 12);
  const binary = Buffer.concat([Buffer.from('\x7fELF…'), payload, header, Buffer.from('\n---- Bun! ----\n'), Buffer.alloc(64)]);
  assert.deepEqual(readBunModules(binary), new Map(files));
  assert.throws(() => readBunModules(Buffer.from('not bun')), /no `---- Bun! ----` trailer/);
});
