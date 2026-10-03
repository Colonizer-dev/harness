// The path-policy matcher (pathpolicy.mjs) against a real temp workspace, since the symlink walk
// is the part a stub cannot stand in for.

import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, symlinkSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { after, before, describe, it } from 'node:test';
import { join } from 'node:path';

import {
  evaluatePathPolicy,
  loadPathPolicy,
  matchPathPolicy,
  parsePathPolicy,
  resolveUnderWorkspace,
} from '../pathpolicy.mjs';

const BIND_LIST = [
  '# written by the boot',
  'mask-file .env',
  'mask-dir .claude/',
  'protect .git/config',
  'protect .vscode/',
  '',
].join('\n');

describe('parsePathPolicy', () => {
  it('reads the bind list the boot writes, trailing slashes trimmed', () => {
    const { masked, protected: protected_, skipped } = parsePathPolicy(BIND_LIST);
    assert.deepEqual(masked, ['.env', '.claude']);
    assert.deepEqual(protected_, ['.git/config', '.vscode']);
    assert.deepEqual(skipped, []);
  });

  it('skips lines the boot could never have written', () => {
    const { masked, protected: protected_, skipped } = parsePathPolicy(
      ['mask-file .env', 'mask .env', 'mask-file /abs.env', 'mask-file .envrc:tok', 'mask-file ..', 'mask-file .env/../x', 'mask-file .git/config', 'protect .git/hooks/', ''].join('\n'),
    );
    assert.deepEqual(masked, ['.env']);
    assert.deepEqual(protected_, ['.git/hooks']);
    assert.equal(skipped.length, 6);
  });
});

describe('loadPathPolicy', () => {
  it('reads the mounted bind list, and an override env wins for tests', () => {
    const dir = mkdtempSync(join(tmpdir(), 'pathpolicy-'));
    try {
      const file = join(dir, 'path-policy');
      writeFileSync(file, BIND_LIST);
      const { policy, warnings } = loadPathPolicy({ COLONIZER_PATH_POLICY: file });
      assert.deepEqual(policy, { masked: ['.env', '.claude'], protected: ['.git/config', '.vscode'] });
      assert.deepEqual(warnings, []);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it('a missing file means the feature is off, silently', () => {
    const { policy, warnings } = loadPathPolicy({ COLONIZER_PATH_POLICY: join(tmpdir(), 'pathpolicy-absent') });
    assert.equal(policy, null);
    assert.deepEqual(warnings, []);
  });

  it('a malformed line is skipped with a warning, not fatal', () => {
    const dir = mkdtempSync(join(tmpdir(), 'pathpolicy-'));
    try {
      const file = join(dir, 'path-policy');
      writeFileSync(file, 'mask-file .env\nwhatever\n');
      const { policy, warnings } = loadPathPolicy({ COLONIZER_PATH_POLICY: file });
      assert.deepEqual(policy.masked, ['.env']);
      assert.equal(warnings.length, 1);
      assert.match(warnings[0], /ignoring bind-list line "whatever"/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe('resolveUnderWorkspace', () => {
  let workspace;

  before(() => {
    workspace = realpathSync(mkdtempSync(join(tmpdir(), 'pathpolicy-ws-')));
    mkdirSync(join(workspace, 'sub'));
    writeFileSync(join(workspace, '.env'), 'SECRET=1\n');
    writeFileSync(join(workspace, 'sub', 'note.md'), 'hi\n');
    symlinkSync(join(workspace, 'sub'), join(workspace, 'link'));
    symlinkSync('/etc', join(workspace, 'outside'));
  });

  after(() => {
    rmSync(workspace, { recursive: true, force: true });
  });

  it('resolves relative and absolute spellings to the same relative path', () => {
    assert.equal(resolveUnderWorkspace(workspace, 'sub/note.md'), 'sub/note.md');
    assert.equal(resolveUnderWorkspace(workspace, join(workspace, 'sub', 'note.md')), 'sub/note.md');
    assert.equal(resolveUnderWorkspace(workspace, 'link/note.md'), 'sub/note.md');
  });

  it('walks a missing tail through its longest existing ancestor', () => {
    assert.equal(resolveUnderWorkspace(workspace, 'sub/nope/deeper.env'), 'sub/nope/deeper.env');
  });

  it('null for the workspace root itself, for escapes, and for junk', () => {
    assert.equal(resolveUnderWorkspace(workspace, '.'), null);
    assert.equal(resolveUnderWorkspace(workspace, 'outside/passwd'), null);
    assert.equal(resolveUnderWorkspace(workspace, '../elsewhere'), null);
    assert.equal(resolveUnderWorkspace(workspace, ''), null);
    assert.equal(resolveUnderWorkspace(workspace, undefined), null);
  });
});

describe('resolveUnderWorkspace through dangling symlinks', () => {
  let workspace;
  let outside;

  before(() => {
    workspace = mkdtempSync(join(tmpdir(), 'pathpolicy-dangle-'));
    outside = mkdtempSync(join(tmpdir(), 'pathpolicy-outside-'));
    mkdirSync(join(workspace, '.claude'));
    symlinkSync(join(outside, 'outside.txt'), join(workspace, 'escape'));
    symlinkSync('../../elsewhere.txt', join(workspace, '.claude', 'rel-escape'));
    symlinkSync('.claude/new.json', join(workspace, 'alias'));
    symlinkSync('loop-b', join(workspace, 'loop-a'));
    symlinkSync('loop-a', join(workspace, 'loop-b'));
  });

  after(() => {
    rmSync(workspace, { recursive: true, force: true });
    rmSync(outside, { recursive: true, force: true });
  });

  it('null for a dangling link that points out of the workspace', () => {
    assert.equal(resolveUnderWorkspace(workspace, 'escape'), null);
    assert.equal(resolveUnderWorkspace(workspace, '.claude/rel-escape'), null);
  });

  it('names the target of a dangling link that stays inside, not the link', () => {
    assert.equal(resolveUnderWorkspace(workspace, 'alias'), '.claude/new.json');
  });

  it('null for a symlink loop', () => {
    assert.equal(resolveUnderWorkspace(workspace, 'loop-a'), null);
    assert.equal(resolveUnderWorkspace(workspace, 'loop-a/x'), null);
  });
});

describe('matchPathPolicy', () => {
  const policy = parsePathPolicy(BIND_LIST);

  it('matches whole components, anchored at the root', () => {
    assert.equal(matchPathPolicy(policy, '.env'), 'masked');
    assert.equal(matchPathPolicy(policy, '.envrc'), null, '.env must not match .envrc');
    assert.equal(matchPathPolicy(policy, 'vendor/.env'), null, 'binds sit at the root; a nested .env is not bound');
    assert.equal(matchPathPolicy(policy, '.claude/settings.json'), 'masked');
    assert.equal(matchPathPolicy(policy, '.git/config'), 'protected');
    assert.equal(matchPathPolicy(policy, '.git/config.bak'), null);
    assert.equal(matchPathPolicy(policy, '.vscode/launch.json'), 'protected');
    assert.equal(matchPathPolicy(policy, 'src/main.rs'), null);
  });
});

describe('evaluatePathPolicy', () => {
  const policy = parsePathPolicy(BIND_LIST);
  let workspace;

  before(() => {
    workspace = realpathSync(mkdtempSync(join(tmpdir(), 'pathpolicy-eval-')));
    mkdirSync(join(workspace, '.git'), { recursive: true });
    mkdirSync(join(workspace, '.vscode'), { recursive: true });
    mkdirSync(join(workspace, '.claude'), { recursive: true });
    writeFileSync(join(workspace, '.env'), 'SECRET=1\n');
    writeFileSync(join(workspace, '.git', 'config'), '[core]\n');
    writeFileSync(join(workspace, '.vscode', 'launch.json'), '{}\n');
    writeFileSync(join(workspace, '.claude', 'settings.json'), '{}\n');
  });

  after(() => {
    rmSync(workspace, { recursive: true, force: true });
  });

  const evaluate = (tool, input) => evaluatePathPolicy(policy, tool, input, { workspace });

  it('reports a masked read with the tool and the relative path', () => {
    assert.deepEqual(evaluate('Read', { file_path: '.env' }), {
      type: 'path_policy',
      access: 'read',
      policy: 'masked',
      path: '.env',
      tool: 'Read',
    });
  });

  it('reports masked or protected writes, but not reads of protected paths', () => {
    assert.deepEqual(evaluate('Edit', { file_path: '.git/config' }), {
      type: 'path_policy',
      access: 'write',
      policy: 'protected',
      path: '.git/config',
      tool: 'Edit',
    });
    assert.equal(evaluate('Read', { file_path: '.git/config' }), null);
    assert.equal(evaluate('Grep', { path: '.git/config' }), null);
    assert.equal(evaluate('Glob', { path: '.vscode/launch.json' }), null, 'a read of protected is allowed');
  });

  it('covers every path-taking tool through its own input field', () => {
    assert.equal(evaluate('MultiEdit', { file_path: '.env' })?.access, 'write');
    assert.equal(evaluate('Write', { file_path: '.claude/settings.json' })?.policy, 'masked');
    assert.equal(evaluate('NotebookEdit', { notebook_path: '.env' })?.access, 'write');
    assert.equal(evaluate('Grep', { path: '.env' })?.access, 'read');
    assert.equal(evaluate('Glob', { path: '.env' })?.access, 'read');
  });

  it('null for tools that take no path, for absent paths, and for paths outside the workspace', () => {
    assert.equal(evaluate('Bash', { command: 'cat .env' }), null, 'shell commands are the exec policy’s business');
    assert.equal(evaluate('Read', {}), null);
    assert.equal(evaluate('Read', { file_path: '/etc/passwd' }), null);
  });

  it('null when the policy is off', () => {
    assert.equal(evaluatePathPolicy(null, 'Read', { file_path: '.env' }, { workspace }), null);
  });

  it('sees a masked target through an alias: the report names the resolved path', () => {
    const workspace = realpathSync(mkdtempSync(join(tmpdir(), 'pathpolicy-eval-')));
    try {
      writeFileSync(join(workspace, '.env'), 'SECRET=1\n');
      symlinkSync('.env', join(workspace, 'secrets.env'));
      const event = evaluatePathPolicy(policy, 'Read', { file_path: 'secrets.env' }, { workspace });
      assert.deepEqual(event, { type: 'path_policy', access: 'read', policy: 'masked', path: '.env', tool: 'Read' });
    } finally {
      rmSync(workspace, { recursive: true, force: true });
    }
  });
});
