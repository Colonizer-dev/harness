// The bench's own check for the readme-typo task.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

test('the typo is gone and the word is still there', () => {
  const readme = readFileSync('README.md', 'utf8');
  assert.ok(!/greeeting/i.test(readme), 'README still says greeeting');
  assert.ok(/greeting/i.test(readme), 'README no longer mentions a greeting at all');
});
