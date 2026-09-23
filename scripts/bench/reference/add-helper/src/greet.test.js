import assert from 'node:assert/strict';
import { test } from 'node:test';

import { greet, shout } from './greet.js';

test('greet names the person', () => {
  assert.equal(greet('Ada'), 'Hello, Ada!');
});

test('shout is the greeting in upper case', () => {
  assert.equal(shout('Ada'), 'HELLO, ADA!');
});
