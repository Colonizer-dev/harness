// For a release bundle (scripts/install.sh --bundle): takes the named packages out of an installed module's
// node_modules and records where scripts/install-release.sh fetches them, so the release does not
// redistribute them. Run from the module directory:
//
//   node record-fetch-at-install.mjs node_modules/@anthropic-ai/claude-agent-sdk ...
//
// Each line of fetch-at-install is `<path> <tarball url> <sha256>`. The tarball is the one package-lock.json
// resolves, checked against the lockfile's integrity before its sha256 is written down, so the installer
// verifies exactly what npm would have installed.
import { createHash } from 'node:crypto';
import { readFileSync, rmSync, writeFileSync } from 'node:fs';

const lock = JSON.parse(readFileSync('package-lock.json', 'utf8'));
const lines = [];
for (const path of process.argv.slice(2)) {
  const entry = lock.packages?.[path];
  if (!entry?.resolved || !entry?.integrity) throw new Error(`${path} has no resolved tarball and integrity in package-lock.json`);
  const [algorithm, expected] = entry.integrity.split(/-(.*)/s);
  const response = await fetch(entry.resolved);
  if (!response.ok) throw new Error(`${entry.resolved}: HTTP ${response.status}`);
  const tarball = Buffer.from(await response.arrayBuffer());
  const actual = createHash(algorithm).update(tarball).digest('base64');
  if (actual !== expected) throw new Error(`${path}: the tarball does not match package-lock.json's integrity`);
  lines.push(`${path} ${entry.resolved} ${createHash('sha256').update(tarball).digest('hex')}`);
  rmSync(path, { recursive: true, force: true });
  console.log(`recorded ${path} ${entry.version} for install time`);
}
writeFileSync('fetch-at-install', `${lines.join('\n')}\n`);
