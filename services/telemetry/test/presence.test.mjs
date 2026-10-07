import assert from 'node:assert/strict';
import { test } from 'node:test';

import { CELL_KM, cellOf, installKey, parseHeartbeat, presenceView } from '../src/presence.js';

const ID = '0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b';

test('a heartbeat keeps only the listed fields', () => {
  const beat = parseHeartbeat({ install_id: ID, version: '0.1.3', platform: 'darwin-arm64', colonies: 2, repo: 'secret/repo', user: 'me' });
  assert.deepEqual(beat, { installId: ID, online: true, version: '0.1.3', platform: 'darwin-arm64', colonies: 2 });
});

test('a heartbeat is refused when a field is wrong', () => {
  assert.match(parseHeartbeat(null).error, /object/);
  assert.match(parseHeartbeat({ install_id: 'not-a-uuid', version: '0.1.3', platform: 'darwin-arm64', colonies: 0 }).error, /install_id/);
  assert.match(parseHeartbeat({ install_id: ID, version: '<script>', platform: 'darwin-arm64', colonies: 0 }).error, /version/);
  assert.match(parseHeartbeat({ install_id: ID, version: '0.1.3', platform: 'windows', colonies: 0 }).error, /platform/);
  assert.match(parseHeartbeat({ install_id: ID, version: '0.1.3', platform: 'darwin-arm64', colonies: 999 }).error, /colonies/);
  assert.match(parseHeartbeat({ install_id: ID, version: '0.1.3', platform: 'darwin-arm64', colonies: 1.5 }).error, /colonies/);
});

test('switching off needs only the install id', () => {
  assert.deepEqual(parseHeartbeat({ install_id: ID, online: false }), { installId: ID, online: false });
});

function kmBetween(a, b) {
  const rad = Math.PI / 180;
  const dLat = (b.lat - a.lat) * rad;
  const dLon = (b.lon - a.lon) * rad;
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a.lat * rad) * Math.cos(b.lat * rad) * Math.sin(dLon / 2) ** 2;
  return 2 * 6371 * Math.asin(Math.sqrt(h));
}

test('a location becomes the centre of its ~25 km cell, nowhere more precise', () => {
  for (const place of [
    { lat: 52.3676, lon: 4.9041 }, // Amsterdam
    { lat: -8.6705, lon: 115.2126 }, // Denpasar
    { lat: 64.1466, lon: -21.9426 }, // Reykjavík
    { lat: 37.7749, lon: -122.4194 }, // San Francisco
    { lat: -33.8688, lon: 151.2093 }, // Sydney
  ]) {
    const cell = cellOf(String(place.lat), String(place.lon));
    // Within one cell of the real place, so the dot is in the right area...
    assert.ok(kmBetween(place, cell) <= CELL_KM, `${JSON.stringify(place)} -> ${JSON.stringify(cell)}`);
    // ...and every place in the same cell gets exactly the same dot.
    const nearby = cellOf(String(cell.lat + 0.01), String(cell.lon + 0.01));
    assert.deepEqual(nearby, cell);
  }
});

test('two addresses a few km apart in one cell are indistinguishable', () => {
  const a = cellOf('52.3700', '4.8900');
  const b = cellOf('52.3780', '4.9300');
  assert.deepEqual(a, b);
});

test('no location, or a nonsense one, gives no cell', () => {
  assert.equal(cellOf(undefined, undefined), null);
  assert.equal(cellOf('abc', '4.9'), null);
  assert.equal(cellOf('95', '4.9'), null);
});

test('the stored key is a hash, not the install id', async () => {
  const key = await installKey(ID);
  assert.match(key, /^[0-9a-f]{64}$/);
  assert.ok(!key.includes(ID.replaceAll('-', '')));
  assert.equal(await installKey(ID), key);
});

test('the public view counts per cell and never lists an install', () => {
  const view = presenceView(
    [
      { cell_lat: 52.4, cell_lon: 4.9, motherships: 2, colonies: 3 },
      { cell_lat: null, cell_lon: null, motherships: 1, colonies: 0 },
    ],
    1_789_000_000,
  );
  assert.equal(view.motherships, 3);
  assert.equal(view.colonies, 3);
  assert.deepEqual(view.cells, [{ lat: 52.4, lon: 4.9, motherships: 2, colonies: 3 }]);
  assert.equal(view.cell_km, 25);
  assert.ok(!JSON.stringify(view).includes('install'));
});
