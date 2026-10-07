import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import {
  ConditionalInstructions,
  INSTRUCTIONS_TOML,
  MAX_FRAGMENT_BYTES,
  MAX_RECENT_PATHS,
  PATH_TOOLS_MATCHER,
  globMatches,
  loadInstructionsConfig,
  parseInstructionsToml,
  parseLabels,
  relInside,
  tokensFromText,
} from '../instructions.mjs';
import { AsyncQueue, buildOptions, runAgent } from '../runner.mjs';

const edit = (file_path, agent_id) => ({ tool_name: 'Edit', tool_input: { file_path }, ...(agent_id ? { agent_id } : {}) });

/** A colony test repo: a billing FOOTGUNS file, a web style rule and a label rule. */
function makeRepo() {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-instructions-'));
  for (const dir of ['billing', 'web', 'docs', '.colonizer']) mkdirSync(join(root, dir), { recursive: true });
  writeFileSync(join(root, 'AGENTS.md'), 'root agents: already loaded as project instructions\n');
  writeFileSync(join(root, 'billing/FOOTGUNS.md'), 'Never divide money by floats.\n');
  writeFileSync(join(root, 'billing/x.ts'), 'export const total = 0;\n');
  writeFileSync(join(root, 'web/AGENTS.md'), 'web: components stay under 200 lines.\n');
  writeFileSync(join(root, 'web/app.tsx'), 'export default () => null;\n');
  writeFileSync(join(root, 'docs/STYLE.md'), 'One class per element.\n');
  writeFileSync(join(root, 'docs/LABEL.md'), 'Frontend work needs a screenshot.\n');
  writeFileSync(
    join(root, INSTRUCTIONS_TOML),
    [
      '# repo instruction rules',
      '[[rule]]',
      'file = "docs/STYLE.md"          # relative to the repo root',
      'paths = ["web/**", "*.css"]',
      '',
      '[[rule]]',
      'file = "docs/LABEL.md"',
      'labels = ["frontend"]',
      '',
    ].join('\n'),
  );
  return root;
}

test('globs match like gitignore patterns', () => {
  assert.equal(globMatches('web/**', 'web/app.tsx'), true);
  assert.equal(globMatches('web/**', 'web/sub/app.tsx'), true, '** crosses separators');
  assert.equal(globMatches('web/**', 'web'), true, 'the directory itself is touched too');
  assert.equal(globMatches('web/**', 'webx/app.tsx'), false);
  assert.equal(globMatches('web/*', 'web/src/App.tsx'), true, 'an ancestor matching drags descendants in');
  assert.equal(globMatches('web/*', 'web/app.tsx'), true);
  assert.equal(globMatches('web/*', 'web'), false);
  assert.equal(globMatches('web', 'web/src/App.tsx'), true, 'a bare directory name covers what is inside it');
  assert.equal(globMatches('*.tsx', 'web/app.tsx'), true, 'no slash means the basename');
  assert.equal(globMatches('*.tsx', 'app.tsx'), true);
  assert.equal(globMatches('*.tsx', 'web/app.css'), false);
  assert.equal(globMatches('*.css', 'a/b/x.css'), true);
  assert.equal(globMatches('docs/STYLE.md', 'docs/STYLE.md'), true);
  assert.equal(globMatches('docs/STYLE.md', 'docs/OTHER.md'), false);
  assert.equal(globMatches('docs/STYLE.md', 'docs'), false, 'an ancestor of the pattern is not a match');
  assert.equal(globMatches('**/*.test.mjs', 'a/b/c.test.mjs'), true);
  assert.equal(globMatches('**/*.test.mjs', 'c.test.mjs'), true, '**/ also matches at the root');
  assert.equal(globMatches('a/**/b', 'a/x/y/b'), true);
  assert.equal(globMatches('a/**/b', 'a/b'), true);
  assert.equal(globMatches('data?.json', 'data1.json'), true);
  assert.equal(globMatches('data?.json', 'data12.json'), false, '? is one character');
  assert.equal(globMatches('file(1).md', 'file(1).md'), true, 'regex syntax is literal');
  assert.equal(globMatches('file(1).md', 'file1.md'), false);
  assert.equal(globMatches('web/', 'web/app.tsx'), true, 'a trailing slash means the directory');
  assert.equal(globMatches('', 'web/app.tsx'), false);
  assert.equal(globMatches('web/**', ''), false);
});

test('the instructions.toml subset parses, and anything else is an error', () => {
  const { rules } = parseInstructionsToml(
    [
      '# comment',
      '[[rule]]',
      'file = "docs/STYLE.md"',
      'paths = ["web/**", "*.css", "*.scss"]   # trailing comment',
      'labels = ["frontend", "web"]',
      '',
      '[[rule]]',
      'file = "docs/ONE.md"',
      'paths = []',
    ].join('\n'),
  );
  assert.deepEqual(rules, [
    { file: 'docs/STYLE.md', paths: ['web/**', '*.css', '*.scss'], labels: ['frontend', 'web'] },
    { file: 'docs/ONE.md', paths: [], labels: [] },
  ]);

  for (const [bad, why] of [
    ['[[settings]]\nfile = "x.md"', 'unknown table'],
    ['[[rule]]\npaths = ["a"]', 'no file'],
    ['[[rule]]\nfile = "x.md"\nextra = "y"', 'unknown key'],
    ['[[rule]]\nfile = "x.md\npaths = []', 'unterminated string'],
    ['[[rule]]\nfile = "x.md"\npaths = ["a"', 'unterminated array'],
    ['[[rule]]\npaths = ["a"]\n\n[[rule]]\nfile = "b.md"\npaths = []', /line 1: rule 1 has no file/],
    ["[[rule]]\nfile = 'x.md'", 'unsupported value'],
    ['[[rule]]\nfile = x.md', 'unsupported value'],
    ['file = "x.md"', 'key outside [[rule]]'],
    ['[[rule]]\nfile = "x.md"\npaths = [a]', 'array items must be strings'],
    ['[[rule]]\nfile = "a\\qb"', 'unsupported escape'],
  ]) {
    assert.throws(() => parseInstructionsToml(bad), why);
  }
});

test('a missing config is empty; an unparseable one is empty plus one warning', () => {
  assert.deepEqual(loadInstructionsConfig(tmpdir()), { rules: [], warning: null });
  const root = mkdtempSync(join(tmpdir(), 'colonizer-instructions-bad-'));
  try {
    mkdirSync(join(root, '.colonizer'), { recursive: true });
    writeFileSync(join(root, INSTRUCTIONS_TOML), '[[rule]\nfile = "x.md"\n');
    const { rules, warning } = loadInstructionsConfig(root);
    assert.deepEqual(rules, []);
    assert.match(warning, /ignored: line 1/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('paths come from tool inputs, and only inside the workspace', () => {
  const root = makeRepo();
  try {
    assert.deepEqual(relInside(root, join(root, 'billing/x.ts')), 'billing/x.ts');
    assert.deepEqual(relInside(root, 'billing/x.ts'), 'billing/x.ts');
    assert.deepEqual(relInside(root, '../outside'), null);
    assert.deepEqual(relInside(root, '/etc/passwd'), null);
    assert.deepEqual(relInside(root, root), null);
    assert.deepEqual(relInside(root, ''), null);

    assert.deepEqual(tokensFromText(`cat billing/x.ts -v 'AGENTS.md'`, { workspace: root }), ['billing/x.ts', 'AGENTS.md']);
    assert.deepEqual(tokensFromText('nothing here matches', { workspace: root }), []);
    // Only the first tokens are examined, so a huge prompt stays cheap.
    const hundred = Array.from({ length: 100 }, (_, i) => `f${i}.ts`).join(' ');
    assert.deepEqual(tokensFromText(hundred, { workspace: root }), []);
    const withHit = Array.from({ length: 100 }, (_, i) => (i === 63 ? 'AGENTS.md' : `f${i}.ts`)).join(' ');
    assert.deepEqual(tokensFromText(withHit, { workspace: root }), ['AGENTS.md'], 'a hit inside the cap is still found');

    assert.deepEqual(parseLabels(' frontend ,backend,, '), ['frontend', 'backend']);
    assert.deepEqual(parseLabels(undefined), []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('editing billing loads billing FOOTGUNS.md once; web loads the style guide; root AGENTS.md never', () => {
  const root = makeRepo();
  try {
    const logs = [];
    const instructions = new ConditionalInstructions({ workspace: root, log: (e) => logs.push(e) });

    const first = instructions.preToolUse(edit(join(root, 'billing/x.ts')));
    assert.equal(first.continue, true);
    assert.equal(first.hookSpecificOutput.hookEventName, 'PreToolUse');
    assert.equal(first.hookSpecificOutput.permissionDecision, undefined, 'never a permission decision');
    assert.match(first.hookSpecificOutput.additionalContext, /^<conditional-instructions file="billing\/FOOTGUNS\.md" reason="billing\/x\.ts">/);
    assert.match(first.hookSpecificOutput.additionalContext, /Never divide money by floats\./);
    assert.ok(logs.some((e) => e.level === 'info' && /loaded billing\/FOOTGUNS\.md \(matched billing\/x\.ts\) \[agent main/.test(e.message)));

    const second = instructions.preToolUse(edit(join(root, 'billing/x.ts')));
    assert.equal(second.hookSpecificOutput, undefined, 'once per fragment per window');

    // Outside the workspace is not a touched path, and injects nothing.
    assert.deepEqual(instructions.preToolUse(edit('/etc/passwd')), { continue: true });

    const web = instructions.preToolUse(edit(join(root, 'web/app.tsx')));
    const context = web.hookSpecificOutput.additionalContext;
    assert.match(context, /file="web\/AGENTS\.md"/);
    assert.match(context, /file="docs\/STYLE\.md" reason="web\/app\.tsx"/);
    assert.match(context, /One class per element\./);
    assert.ok(!context.includes('root agents'), 'the root AGENTS.md is already the project instructions');

    // A subagent's window is its own: it has not seen any of this.
    const sub = instructions.preToolUse(edit(join(root, 'billing/x.ts'), 'sub-1'));
    assert.match(sub.hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);
    const subAgain = instructions.preToolUse(edit(join(root, 'billing/x.ts'), 'sub-1'));
    assert.equal(subAgain.hookSpecificOutput, undefined, 'and neither does it see it twice');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('a compaction re-injects what still holds and drops the rest', () => {
  const root = makeRepo();
  try {
    const logs = [];
    const instructions = new ConditionalInstructions({ workspace: root, log: (e) => logs.push(e) });
    instructions.preToolUse(edit(join(root, 'billing/x.ts')));
    instructions.preToolUse(edit(join(root, 'web/app.tsx')));

    const start = instructions.sessionStart({ source: 'compact' });
    assert.equal(start.hookSpecificOutput.hookEventName, 'SessionStart');
    const context = start.hookSpecificOutput.additionalContext;
    assert.match(context, /file="billing\/FOOTGUNS\.md"/);
    assert.match(context, /file="docs\/STYLE\.md"/);
    assert.ok(
      logs.some((e) => e.level === 'info' && e.message.includes('reloaded after compaction') && e.message.includes('billing/FOOTGUNS.md') && e.message.includes('docs/STYLE.md')),
      logs.map((e) => e.message).join('\n'),
    );

    // After enough other paths the conditions no longer hold, so a compaction drops them…
    for (let i = 0; i <= MAX_RECENT_PATHS; i++) instructions.preToolUse(edit(join(root, `scratch/f${i}.ts`)));
    instructions.markCompacted();
    const next = instructions.preToolUse(edit(join(root, 'scratch/after.ts')));
    assert.equal(next.hookSpecificOutput, undefined, 'no condition holds any more');
    assert.ok(!logs.some((e) => e.message.includes('reloaded after compaction: billing')), 'the dropped fragment is not reloaded');

    // …and a dropped fragment comes back when its condition holds again.
    const back = instructions.preToolUse(edit(join(root, 'billing/x.ts')));
    assert.match(back.hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('the compact_boundary fallback re-injects without doubling up on SessionStart', () => {
  const root = makeRepo();
  try {
    const instructions = new ConditionalInstructions({ workspace: root });
    instructions.preToolUse(edit(join(root, 'billing/x.ts')));
    // SessionStart ran for the compaction: the fallback must not inject the same text again.
    instructions.sessionStart({ source: 'compact' });
    instructions.markCompacted();
    assert.equal(instructions.preToolUse(edit(join(root, 'billing/x.ts'))).hookSpecificOutput, undefined);

    // Without a SessionStart the fallback does the re-injecting at the next hook call.
    const alone = new ConditionalInstructions({ workspace: root });
    alone.preToolUse(edit(join(root, 'billing/x.ts')));
    alone.markCompacted();
    assert.match(alone.preToolUse(edit(join(root, 'scratch/now.ts'))).hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('a label rule holds from the prompt on, without any path', () => {
  const root = makeRepo();
  try {
    const instructions = new ConditionalInstructions({ workspace: root, labels: ['frontend'] });
    const injected = instructions.userPromptSubmit({ prompt: 'please start the work' });
    assert.equal(injected.hookSpecificOutput.hookEventName, 'UserPromptSubmit');
    assert.match(injected.hookSpecificOutput.additionalContext, /file="docs\/LABEL\.md" reason="label frontend"/);
    assert.doesNotMatch(injected.hookSpecificOutput.additionalContext, /file="docs\/STYLE\.md"/, 'the path rule has no path yet');
    assert.equal(instructions.userPromptSubmit({ prompt: 'carry on' }).hookSpecificOutput, undefined, 'once per window');

    const unlabelled = new ConditionalInstructions({ workspace: root });
    assert.equal(unlabelled.userPromptSubmit({ prompt: 'please start the work' }).hookSpecificOutput, undefined, 'without the label the rule does not hold');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('Bash commands and prompt text count as touching the paths they name', () => {
  const root = makeRepo();
  try {
    const viaBash = new ConditionalInstructions({ workspace: root }).preToolUse({
      tool_name: 'Bash',
      tool_input: { command: 'cat billing/x.ts | grep total' },
    });
    assert.match(viaBash.hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);

    const viaPrompt = new ConditionalInstructions({ workspace: root }).userPromptSubmit({
      prompt: 'have a look at billing/x.ts before anything else',
    });
    assert.match(viaPrompt.hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);

    // A capped fragment carries a truncation marker, not the whole file.
    writeFileSync(join(root, 'docs/BIG.md'), `${'x'.repeat(MAX_FRAGMENT_BYTES + 100)}\n`);
    writeFileSync(join(root, INSTRUCTIONS_TOML), '[[rule]]\nfile = "docs/BIG.md"\npaths = ["**"]\n');
    const capped = new ConditionalInstructions({ workspace: root }).preToolUse(edit(join(root, 'billing/x.ts')));
    const text = capped.hookSpecificOutput.additionalContext;
    assert.ok(text.includes('truncated'), 'the cut is marked');
    assert.ok(text.length < 2 * MAX_FRAGMENT_BYTES, 'one fragment does not flood the window');

    // The cut never splits a UTF-8 character in half.
    writeFileSync(join(root, 'docs/WIDE.md'), `${'a'.repeat(MAX_FRAGMENT_BYTES - 1)}${'🎉'.repeat(20)}`);
    writeFileSync(join(root, INSTRUCTIONS_TOML), '[[rule]]\nfile = "docs/WIDE.md"\npaths = ["**"]\n');
    const wide = new ConditionalInstructions({ workspace: root }).preToolUse(edit(join(root, 'billing/x.ts')));
    const wideText = wide.hookSpecificOutput.additionalContext;
    assert.ok(!wideText.includes('�'), `no broken character: ${JSON.stringify(wideText.slice(-40))}`);
    assert.ok(wideText.includes('truncated'));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('fragments outside the workspace are refused with one warning, and a broken config never throws', () => {
  const parent = mkdtempSync(join(tmpdir(), 'colonizer-instructions-esc-'));
  const root = join(parent, 'repo');
  mkdirSync(join(root, '.colonizer'), { recursive: true });
  mkdirSync(join(root, 'billing'), { recursive: true });
  try {
    writeFileSync(join(parent, 'secret.md'), 'offsite guidance\n');
    symlinkSync(join(parent, 'secret.md'), join(root, 'link.md'));
    writeFileSync(
      join(root, INSTRUCTIONS_TOML),
      '[[rule]]\nfile = "../secret.md"\npaths = ["**"]\n\n[[rule]]\nfile = "link.md"\npaths = ["**"]\n',
    );
    writeFileSync(join(root, 'billing/FOOTGUNS.md'), 'inside\n');

    const logs = [];
    const instructions = new ConditionalInstructions({ workspace: root, log: (e) => logs.push(e) });
    const first = instructions.preToolUse(edit(join(root, 'billing/x.ts')));
    assert.match(first.hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);
    assert.ok(!first.hookSpecificOutput.additionalContext.includes('offsite guidance'), 'the escape is refused');
    const refusals = logs.filter((e) => e.level === 'warn' && e.message.includes('refused: it resolves outside the workspace'));
    assert.equal(refusals.length, 2, `both escapes are named once: ${refusals.map((e) => e.message).join(' | ')}`);

    // A malformed config is skipped with one warning; directory fragments still work.
    writeFileSync(join(root, INSTRUCTIONS_TOML), '[[rule]]\nnope\n');
    const broken = new ConditionalInstructions({ workspace: root, log: (e) => logs.push(e) });
    const after = broken.preToolUse(edit(join(root, 'billing/x.ts')));
    assert.match(after.hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);
    assert.ok(logs.some((e) => e.level === 'warn' && e.message.includes('ignored: line 2')));
  } finally {
    rmSync(parent, { recursive: true, force: true });
  }
});

test('buildOptions registers the hooks behind the gates, and nothing without a tracker', async () => {
  const root = makeRepo();
  try {
    const bare = buildOptions({ COLONIZER_DELEGATE: 'off' }).options;
    // Only the exec policy's Bash hook (#471), which every colony gets; no instruction hooks.
    assert.deepEqual(bare.hooks.PreToolUse.map((e) => e.matcher), ['Bash']);
    assert.equal(bare.hooks.UserPromptSubmit, undefined);
    assert.equal(bare.hooks.SessionStart, undefined);

    const instructions = new ConditionalInstructions({ workspace: root });
    const { options } = buildOptions({ COLONIZER_DELEGATE: 'enforce' }, { instructions });
    // Delegation gate first, then the exec policy's Bash hook (#471), then the instructions hook last.
    assert.equal(options.hooks.PreToolUse.length, 3, 'the delegation gate stays first');
    assert.equal(options.hooks.PreToolUse[1].matcher, 'Bash');
    assert.equal(options.hooks.PreToolUse.at(-1).matcher, PATH_TOOLS_MATCHER);
    assert.ok(options.hooks.UserPromptSubmit.length >= 1);
    assert.ok(options.hooks.SessionStart.length >= 1);

    // The gate keeps its deny; the instructions hook still only adds context.
    const gate = await options.hooks.PreToolUse[0].hooks[0]({ tool_name: 'Write', tool_input: { file_path: '/workspace/src/main.rs' } });
    assert.equal(gate.hookSpecificOutput.permissionDecision, 'deny');
    const injected = await options.hooks.PreToolUse.at(-1).hooks[0](edit(join(root, 'billing/x.ts')));
    assert.match(injected.hookSpecificOutput.additionalContext, /file="billing\/FOOTGUNS\.md"/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('runAgent tells the tracker about a compaction it sees', async () => {
  let marked = 0;
  const instructions = { markCompacted: () => (marked += 1) };
  const query = ({ prompt }) =>
    (async function* () {
      for await (const _ of prompt) {
        yield { type: 'system', subtype: 'init', session_id: 's1', model: 'fake-model' };
        yield { type: 'system', subtype: 'compact_boundary', compact_metadata: { trigger: 'manual', pre_tokens: 100, post_tokens: 10 } };
        yield { type: 'result', subtype: 'success', is_error: false, result: 'ok', total_cost_usd: 0, duration_ms: 1 };
      }
    })();
  const commands = new AsyncQueue();
  const emit = (event) => {
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', text: 'go' });
  await runAgent({ query, commands, emit, options: {}, graceMs: 100, instructions });
  assert.equal(marked, 1);
});
