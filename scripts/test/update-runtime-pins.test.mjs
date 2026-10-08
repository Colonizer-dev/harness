import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  compareVersions,
  formatUnderstandAnythingLock,
  parseUnderstandAnythingLock,
  withTrailingNewline,
} from '../update-runtime-pins.mjs';

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

const SHA = 'a'.repeat(64);
const COMMIT = 'b'.repeat(40);
const LINE = `understand-anything  v2.9.0  any  source  ${SHA}  https://codeload.github.com/Egonex-AI/Understand-Anything/tar.gz/${COMMIT}`;

// The skillset row is the one lock line a proposal rewrites whole (the version and the URL both move),
// so parse/format have to agree on it or the daily job writes a row the mothership then refuses.
test('an understand-anything lock row round-trips', () => {
  assert.deepEqual(parseUnderstandAnythingLock(LINE), {
    name: 'understand-anything',
    version: 'v2.9.0',
    platform: 'any',
    kind: 'source',
    sha: SHA,
    url: `https://codeload.github.com/Egonex-AI/Understand-Anything/tar.gz/${COMMIT}`,
  });
  const fresh = formatUnderstandAnythingLock({ version: 'v3.0.0', sha: 'c'.repeat(64), commit: 'd'.repeat(40) });
  assert.equal(parseUnderstandAnythingLock(fresh).version, 'v3.0.0');
  assert.ok(fresh.endsWith(`/tar.gz/${'d'.repeat(40)}`), 'the URL pins the commit, not the tag');
});

test('a row that is not the skillset\'s is not parsed as one', () => {
  assert.equal(parseUnderstandAnythingLock('# a header comment'), null);
  assert.equal(parseUnderstandAnythingLock('   '), null);
  assert.equal(parseUnderstandAnythingLock(''), null);
  assert.equal(parseUnderstandAnythingLock(undefined), null);
  assert.equal(parseUnderstandAnythingLock('claude-code  2.1.285  linux-x64  agent  ' + SHA + '  https://example.com/claude'), null, 'another skillset');
  assert.equal(parseUnderstandAnythingLock(LINE.replace(SHA, 'not-a-sha')), null, 'a checksum is never guessed');
  assert.equal(parseUnderstandAnythingLock(`${LINE} extra`), null, 'seven columns are not a lock row');
  assert.equal(parseUnderstandAnythingLock(LINE.replace('codeload.github.com', 'example.com')), null, 'the tarball is the only source pinned');
});

// --write joins the rewritten lines back together, and a lock file with no trailing newline comes
// out of git that way; the rewrite puts it back, because a lock without one shows up as a "\ No
// newline at end of file" in the very diff the person merging it is meant to read.
test('a rewritten lock file keeps its trailing newline', () => {
  assert.equal(withTrailingNewline(LINE), `${LINE}\n`);
  assert.equal(withTrailingNewline(`${LINE}\n`), `${LINE}\n`, 'one is not two');
});
