import assert from 'node:assert/strict';
import { test } from 'node:test';

import { greet } from './greet.js';

test('greet names the person', () => {
  assert.equal(greet('Ada'), 'Hello, Ada!');
});
