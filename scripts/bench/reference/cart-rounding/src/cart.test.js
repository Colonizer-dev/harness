import assert from 'node:assert/strict';
import { test } from 'node:test';

import { total } from './cart.js';

test('a cart of whole euros adds up', () => {
  assert.equal(total([{ price: 2, quantity: 3 }, { price: 5, quantity: 1 }]), 11);
});

test('prices in cents add up exactly', () => {
  assert.equal(total([{ price: 0.1, quantity: 1 }, { price: 0.2, quantity: 1 }, { price: 0.1, quantity: 1 }]), 0.4);
  assert.equal(total([{ price: 1.15, quantity: 3 }]), 3.45);
});
