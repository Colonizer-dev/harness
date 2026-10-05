// The colony-node image (images/colony-node/Dockerfile) is how bun and pnpm repositories become
// verifiable (#589): a done-claim check that needs bun or pnpm reports unverifiable on an image
// without them. Until the published image is public and pinned in crates/colonizer/images.lock, the
// node preset keeps stock node:24-bookworm, so this pins what the image promises from here: exact
// bun and pnpm versions, each per-arch download checked against a sha256, a build that fails if a
// tool does not run, and a base that is the lock's node digest. It also checks
// images/anonymous-pull.sh, the publish job's test that a pushed digest is safe to pin.
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const read = (path) => readFileSync(join(ROOT, path), 'utf8');
const DOCKERFILE = read('images/colony-node/Dockerfile');
const SHA = /^[0-9a-f]{64}$/;

/** The default of `ARG name=value` in the Dockerfile. */
function arg(name) {
  const match = DOCKERFILE.match(new RegExp(`^ARG ${name}=(\\S+)$`, 'm'));
  assert.ok(match, `images/colony-node/Dockerfile has no ARG ${name}=…`);
  return match[1];
}

test('bun and pnpm are pinned to exact versions', () => {
  assert.match(arg('BUN_VERSION'), /^\d+\.\d+\.\d+$/, 'BUN_VERSION is not an exact version');
  assert.match(arg('PNPM_VERSION'), /^\d+\.\d+\.\d+$/, 'PNPM_VERSION is not an exact version');
});

test('every download the image takes is checked against a sha256, per architecture', () => {
  for (const name of ['BUN_SHA256_amd64', 'BUN_SHA256_arm64', 'PNPM_SHA256', 'PNPM_EXE_SHA256_amd64', 'PNPM_EXE_SHA256_arm64']) {
    assert.match(arg(name), SHA, `${name} is not a sha256`);
  }
  // Each fetched file goes through sha256sum -c before it is used: bun's zip, pnpm's wrapper and
  // its native binary.
  for (const file of ['/tmp/bun.zip', '/tmp/pnpm.tgz', '/tmp/pnpm-exe.tgz']) {
    assert.ok(DOCKERFILE.includes(`}  ${file}" | sha256sum -c -`), `${file} is not checksum-verified`);
  }
  // npm must not fetch pnpm's platform binary on its own, unpinned.
  assert.match(DOCKERFILE, /npm install -g --ignore-scripts --omit=optional/);
});

test('the build fails unless bun, pnpm and yarn each run', () => {
  assert.match(DOCKERFILE, /^RUN bun --version && pnpm --version && yarn --version$/m);
});

test("the base is images.lock's node digest", () => {
  const from = DOCKERFILE.match(/^FROM node:24-bookworm@sha256:([0-9a-f]{64})$/m);
  assert.ok(from, 'images/colony-node/Dockerfile does not FROM a digest-pinned node:24-bookworm');
  const row = read('crates/colonizer/images.lock')
    .split('\n')
    .filter((line) => line && !line.startsWith('#'))
    .map((line) => line.split(/\s+/))
    .find((cols) => cols[5] === 'node:24-bookworm' && cols[3] === 'image');
  assert.ok(row, 'images.lock has no node:24-bookworm image row');
  assert.equal(from[1], row[4], 'the colony-node base and images.lock pin different node:24-bookworm digests');
});

/** Runs images/anonymous-pull.sh with a stub curl that answers the manifest request with `code`. */
function anonymousPull(code, digest = `sha256:${'a'.repeat(64)}`) {
  const bin = mkdtempSync(join(tmpdir(), 'anon-pull-'));
  try {
    writeFileSync(
      join(bin, 'curl'),
      `#!/bin/sh\ncase "$*" in *token*) echo '{"token":"anon"}' ;; *) printf '%s' '${code}' ;; esac\n`,
    );
    chmodSync(join(bin, 'curl'), 0o755);
    return spawnSync('sh', [join(ROOT, 'images/anonymous-pull.sh'), 'colonizer-dev/colony-node', digest], {
      encoding: 'utf8',
      env: { ...process.env, PATH: `${bin}:${process.env.PATH}` },
    });
  } finally {
    rmSync(bin, { recursive: true, force: true });
  }
}

test('anonymous-pull.sh says public only when an anonymous pull of the digest succeeds', () => {
  const open = anonymousPull('200');
  assert.equal(open.status, 0, open.stderr);
  assert.equal(open.stdout, 'public=true\n');

  // A private GHCR package answers an anonymous pull with 401 or 403: reported, not failed.
  for (const code of ['401', '403', '404']) {
    const closed = anonymousPull(code);
    assert.equal(closed.status, 0, closed.stderr);
    assert.equal(closed.stdout, 'public=false\n');
    assert.match(closed.stderr, /not pullable anonymously .*make the package public/);
  }

  assert.equal(anonymousPull('200', 'latest').status, 2, 'a tag is not a digest');
});

test('anonymous-pull.sh is valid sh', () => {
  execFileSync('sh', ['-n', join(ROOT, 'images/anonymous-pull.sh')]);
});
