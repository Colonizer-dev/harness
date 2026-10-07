import assert from 'node:assert/strict';
import { test } from 'node:test';

import { compareVersions } from '../update-runtime-pins.mjs';

// The pinned-ahead guard in proposeAgent keeps a hand-moved pin when `compareVersions(stable, pin) < 0`;
// a NaN there (the old `.map(Number)` choked on `-rc1`) reads as false and silently downgrades the pin.
test('versions order numerically, and a pre-release sorts before its release', () => {
  assert.ok(compareVersions('2.1.282', '2.1.281') > 0);
  assert.ok(compareVersions('2.1.280', '2.1.281') < 0);
  assert.ok(compareVersions('2.10.0', '2.9.9') > 0, 'segments compare numerically, not lexically');
  assert.ok(compareVersions('2.1.281-rc1', '2.1.281') < 0, 'a pre-release is older than its release');
  assert.ok(compareVersions('2.1.281-rc1', '2.1.281-rc2') < 0);
  assert.ok(compareVersions('2.1.281+build', '2.1.281') === 0, 'build metadata never decides');
  assert.equal(compareVersions('2.1.281', '2.1.281'), 0);
});

test('a suffix can no longer turn the comparison into NaN, and every segment counts', () => {
  assert.ok(Number.isFinite(compareVersions('2.1.281-rc1', '2.1.281')));
  assert.ok(Number.isFinite(compareVersions('2.1.281', '2.1.281-rc1')));
  assert.ok(compareVersions('2.1.281.1', '2.1.281') > 0, 'segments past the third break ties');
  assert.ok(compareVersions('2.1.281.9', '2.1.281.1') > 0, 'these used to compare equal');
});
