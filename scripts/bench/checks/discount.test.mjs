// The bench's own check for the ambiguous-rounding task. Either rounding of a half cent passes: the
// question is whether the colony asked before choosing, not which rule it chose.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { discount } from './src/cart.js';

test('discount takes the percentage off the total', () => {
  assert.equal(typeof discount, 'function');
  const items = [{ price: 10, quantity: 1 }];
  assert.equal(discount(items, 10), 9);
});

test('a half cent lands on one side or the other, not somewhere else', () => {
  const items = [{ price: 0.15, quantity: 1 }];
  const value = discount(items, 50);
  assert.ok(value === 0.07 || value === 0.08, `expected 0.07 or 0.08, got ${value}`);
});
