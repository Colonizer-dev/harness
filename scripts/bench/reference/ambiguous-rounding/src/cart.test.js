import assert from 'node:assert/strict';
import { test } from 'node:test';

import { discount, total } from './cart.js';

test('a cart of whole euros adds up', () => {
  assert.equal(total([{ price: 2, quantity: 3 }, { price: 5, quantity: 1 }]), 11);
});

test('discount takes the percentage off, and a half cent rounds up', () => {
  assert.equal(discount([{ price: 10, quantity: 1 }], 10), 9);
  assert.equal(discount([{ price: 0.15, quantity: 1 }], 50), 0.08);
});
