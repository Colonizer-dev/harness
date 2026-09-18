// The bench's own check for the add-helper task: copied into the colony's branch and run there.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { greet, shout } from './src/greet.js';

test('shout is the greeting in upper case', () => {
  assert.equal(typeof shout, 'function');
  assert.equal(shout('Ada'), greet('Ada').toUpperCase());
});
