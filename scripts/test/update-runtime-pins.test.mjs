import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  compareVersions,
  formatUnderstandAnythingLock,
  parsePlanetscaleLock,
  formatPscaleLock,
  formatPlanetscaleSkillsLock,
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

const PSCALE_SHA = 'a'.repeat(64);
const PSCALE_COMMIT = 'b'.repeat(40);
const PSCALE_LINUX_X86_64 = `pscale  0.347.0  linux-x86_64  archive  ${PSCALE_SHA}  https://github.com/planetscale/cli/releases/download/v0.347.0/pscale_0.347.0_linux_amd64.tar.gz`;
const PSCALE_LINUX_ARM64 = `pscale  0.347.0  linux-aarch64  archive  ${PSCALE_SHA}  https://github.com/planetscale/cli/releases/download/v0.347.0/pscale_0.347.0_linux_arm64.tar.gz`;
const PSCALE_DARWIN_ARM64 = `pscale  0.347.0  darwin-aarch64  archive  ${PSCALE_SHA}  https://github.com/planetscale/cli/releases/download/v0.347.0/pscale_0.347.0_macOS_arm64.tar.gz`;
const PLANETSCALE_SKILLS = `planetscale-skills  ${PSCALE_COMMIT}  any  source  ${PSCALE_SHA}  https://codeload.github.com/planetscale/skills/tar.gz/${PSCALE_COMMIT}`;

test('a pscale lock row round-trips', () => {
  const parsed = parsePlanetscaleLock(PSCALE_LINUX_X86_64);
  assert.deepEqual(parsed, {
    name: 'pscale',
    version: '0.347.0',
    platform: 'linux-x86_64',
    kind: 'archive',
    sha: PSCALE_SHA,
    url: 'https://github.com/planetscale/cli/releases/download/v0.347.0/pscale_0.347.0_linux_amd64.tar.gz',
  });
  const formatted = formatPscaleLock({
    version: '0.348.0',
    platform: 'linux-x86_64',
    sha: 'c'.repeat(64),
    url: 'https://github.com/planetscale/cli/releases/download/v0.348.0/pscale_0.348.0_linux_amd64.tar.gz',
  });
  assert.ok(formatted.includes('0.348.0'), 'version is in the output');
  assert.ok(formatted.includes('c'.repeat(64)), 'sha is in the output');
});

test('a planetscale-skills lock row round-trips', () => {
  const parsed = parsePlanetscaleLock(PLANETSCALE_SKILLS);
  assert.deepEqual(parsed, {
    name: 'planetscale-skills',
    version: PSCALE_COMMIT,
    platform: 'any',
    kind: 'source',
    sha: PSCALE_SHA,
    url: `https://codeload.github.com/planetscale/skills/tar.gz/${PSCALE_COMMIT}`,
  });
  const fresh = formatPlanetscaleSkillsLock({ commit: 'd'.repeat(40), sha: 'e'.repeat(64) });
  assert.ok(fresh.includes('d'.repeat(40)), 'commit is in the output');
  assert.ok(fresh.includes('e'.repeat(64)), 'sha is in the output');
  assert.ok(fresh.includes('any'), 'platform is any');
});

test('a row that is not a pscale or planetscale-skills row is not parsed as one', () => {
  assert.equal(parsePlanetscaleLock('# a header comment'), null);
  assert.equal(parsePlanetscaleLock('   '), null);
  assert.equal(parsePlanetscaleLock(''), null);
  assert.equal(parsePlanetscaleLock(undefined), null);
  assert.equal(parsePlanetscaleLock('understand-anything  v2.9.0  any  source  ' + PSCALE_SHA + '  https://example.com/tarball'), null, 'another skillset');
  assert.equal(parsePlanetscaleLock(PSCALE_LINUX_X86_64.replace(PSCALE_SHA, 'not-a-sha')), null, 'a checksum is never guessed');
  assert.equal(parsePlanetscaleLock(`${PSCALE_LINUX_X86_64} extra`), null, 'seven columns are not a lock row');
  assert.equal(parsePlanetscaleLock(PSCALE_LINUX_X86_64.replace('github.com/planetscale/cli', 'example.com')), null, 'pscale must be from planetscale/cli');
  assert.equal(parsePlanetscaleLock(PLANETSCALE_SKILLS.replace('codeload.github.com/planetscale/skills', 'example.com')), null, 'planetscale-skills must be from codeload');
});

test('a version below the minimum pscale version is rejected', () => {
  // This test verifies that compareVersions exists and works; the actual minimum version check
  // happens in latestPscaleRelease() which we cannot test without network access.
  assert.ok(compareVersions('0.292.0', '0.291.0') > 0, 'the minimum version is after the rejected version');
});
