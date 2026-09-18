// The bench's own check for the cart-rounding task.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { total } from './src/cart.js';

test('small prices add up exactly', () => {
  assert.equal(total([{ price: 0.1, quantity: 1 }, { price: 0.2, quantity: 1 }, { price: 0.1, quantity: 1 }]), 0.4);
});

test('quantities still multiply', () => {
  assert.equal(total([{ price: 1.15, quantity: 3 }]), 3.45);
});
