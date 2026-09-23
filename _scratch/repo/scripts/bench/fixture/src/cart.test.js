import assert from 'node:assert/strict';
import { test } from 'node:test';

import { total } from './cart.js';

test('a cart of whole euros adds up', () => {
  assert.equal(total([{ price: 2, quantity: 3 }, { price: 5, quantity: 1 }]), 11);
});
