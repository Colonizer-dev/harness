// The one line in public/sw.js the build fills in. public/sw.js ships with the placeholder below —
// `const BUILD = { hash: "dev", assets: [] };` — and the build (vite.config.ts) rewrites it with the
// build's short hash and its emitted /assets list, so the worker can precache the whole build at
// install and keep serving a superseded build's chunks to tabs that have not reloaded yet. Kept out
// of the app bundle on purpose: only the build and the tests import this.

/** The line in public/sw.js that the build rewrites. Must match sw.js exactly. */
export const BUILD_LINE = 'const BUILD = { hash: "dev", assets: [] };';

/**
 * A short stable digest of the build's asset list. Vite's /assets names are content-hashed, so this
 * changes exactly when the build's files do — it names the worker's per-build cache.
 */
export function buildHash(assets: readonly string[]): string {
  let hash = 0x811c9dc5;
  for (const chars of assets) {
    for (let i = 0; i < chars.length; i++) {
      hash ^= chars.charCodeAt(i);
      hash = Math.imul(hash, 0x01000193);
    }
    hash ^= 0x0a; // "\n": a separator, so ["ab", "c"] and ["a", "bc"] hash differently
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

/** The line the build writes over the placeholder. */
export function buildLine(hash: string, assets: readonly string[]): string {
  return `const BUILD = { hash: ${JSON.stringify(hash)}, assets: ${JSON.stringify(assets)} };`;
}

/**
 * The worker source with its BUILD placeholder replaced. Throws when the placeholder is missing or
 * appears twice: a drifted sw.js must fail the build, never ship with every cache named "dev".
 */
export function withBuild(source: string, hash: string, assets: readonly string[]): string {
  const parts = source.split(BUILD_LINE);
  if (parts.length !== 2) {
    throw new Error(`public/sw.js must contain the BUILD placeholder exactly once: ${BUILD_LINE}`);
  }
  return parts.join(buildLine(hash, assets));
}
