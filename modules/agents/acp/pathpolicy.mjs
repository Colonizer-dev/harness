// Runtime path-policy reporting (issue #647). The mount hides masked files and write-protects
// protected ones before the agent's first command, so a blocked read only ever shows up as an
// oddly empty file; this layer makes the ATTEMPT visible instead. The runner checks each
// path-taking tool call against the same bind list the guest booted with and emits one
// `path_policy` event per distinct (access, path). Reporting only — nothing here blocks, and the
// event must never carry a `decision` or permission field: the mount is the boundary and was
// already enforcing when this runs. Pure module — every decision follows from the bind-list text
// and the tool input, so tests drive it without an SDK.

import { readFileSync, realpathSync } from 'node:fs';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';

/** Where the boot mounts the colony's bind list (BOOT_SCRIPT in crates/colonizer/src/boot.rs). */
export const PATH_POLICY_FILE = '/colonizer/path-policy';
const POLICY_MAX_BYTES = 64 * 1024; // a bind list is lines, not data; more is a mistake

// The path-taking tools the SDK surfaces as tool_use blocks, and what each one touches. Reads of
// protected paths are allowed by the policy (the mount only bars writes there), so a read reports
// only masked hits; a write reports masked or protected ones.
const TOOL_PATHS = new Map([
  ['Read', { field: 'file_path', access: 'read' }],
  ['Write', { field: 'file_path', access: 'write' }],
  ['Edit', { field: 'file_path', access: 'write' }],
  ['MultiEdit', { field: 'file_path', access: 'write' }],
  ['NotebookEdit', { field: 'notebook_path', access: 'write' }],
  ['Grep', { field: 'path', access: 'read' }],
  ['Glob', { field: 'path', access: 'read' }],
]);

/**
 * Parses a bind list (the `mask-file` / `mask-dir` / `protect` lines the boot writes to
 * `/colonizer/path-policy`) into `{ masked, protected, skipped }`, with each entry trimmed of its
 * trailing `/`. Lines that name a path the wiring could never carry are skipped, not fatal — the
 * same judgement the host's save-time gate makes (path_policy::validate_path), so a hand-edited
 * override file cannot push matching onto a path the mount never bound. A path under `.git` is
 * skipped for masked entries: the guest never binds one (the git dir is mounted read-only whole),
 * so reporting it would promise an enforcement that is not there.
 * @param {string} text
 * @returns {{ masked: string[], protected: string[], skipped: string[] }}
 */
export function parsePathPolicy(text) {
  const masked = [];
  const protected_ = [];
  const skipped = [];
  const seen = new Set();
  for (const line of String(text ?? '').split('\n')) {
    const entry = line.trim();
    if (!entry || entry.startsWith('#')) continue;
    const spaceAt = entry.indexOf(' ');
    const kind = spaceAt === -1 ? entry : entry.slice(0, spaceAt);
    const path = spaceAt === -1 ? '' : entry.slice(spaceAt + 1).trim();
    if (!['mask-file', 'mask-dir', 'protect'].includes(kind) || !usablePolicyPath(path, kind === 'protect')) {
      skipped.push(line);
      continue;
    }
    const list = kind === 'protect' ? protected_ : masked;
    const bare = path.replace(/\/+$/, '');
    if (!seen.has(`${kind}\u0000${bare}`)) {
      seen.add(`${kind}\u0000${bare}`);
      list.push(bare);
    }
  }
  return { masked, protected: protected_, skipped };
}

/** path_policy::validate_path, in the one direction parsing needs: could the boot have bound this? */
function usablePolicyPath(path, allowGitDir) {
  if (!path || path !== path.trim()) return false;
  if ([...path].some((ch) => isControl(ch) || ch === ':' || ch === ',')) return false;
  if (path.startsWith('/')) return false;
  const bare = path.replace(/\/+$/, '');
  if (!bare || bare === '.') return false;
  if (bare.split('/').some((c) => c === '' || c === '.' || c === '..')) return false;
  if (!allowGitDir && (bare === '.git' || bare.startsWith('.git/'))) return false;
  return true;
}

const isControl = (ch) => ch.codePointAt(0) < 0x20 || ch.codePointAt(0) === 0x7f;

/**
 * Loads the policy the guest enforces: the bind list at [`PATH_POLICY_FILE`] (or
 * `COLONIZER_PATH_POLICY`, how tests point it at a fixture). A missing file means the feature is
 * off — the harness predating the mount — and is silent, not a warning. Malformed lines are
 * skipped with a warning, in the exec-policy style: the layer degrades, the mount does not.
 * @param {NodeJS.ProcessEnv} [env]
 * @param {{ readFile?: (path: string, max: number) => string }} [opts]
 * @returns {{ policy: { masked: string[], protected: string[] } | null, warnings: string[] }}
 */
export function loadPathPolicy(env = process.env, { readFile = readTextCapped } = {}) {
  const file = env.COLONIZER_PATH_POLICY || PATH_POLICY_FILE;
  let text;
  try {
    text = readFile(file, POLICY_MAX_BYTES);
  } catch {
    return { policy: null, warnings: [] };
  }
  const { masked, protected: protected_, skipped } = parsePathPolicy(text);
  return {
    policy: { masked, protected: protected_ },
    warnings: skipped.map((line) => `path policy: ignoring bind-list line ${JSON.stringify(line.slice(0, 200))}`),
  };
}

/**
 * The workspace-relative path `target` names, or null when it does not name one inside the
 * workspace. Symlinks resolve through the longest existing ancestor — the same walk ACP's
 * `confine` makes — so a link pointing out of the tree cannot dress an outside path up as a
 * relative one (and cannot hide a masked target either: the mount binds resolved targets).
 * @param {string} workspace  absolute
 * @param {string} target     as the tool input spelled it, relative or absolute
 * @returns {string | null}
 */
export function resolveUnderWorkspace(workspace, target) {
  let abs = resolve(workspace, String(target ?? ''));
  const tail = [];
  for (;;) {
    try {
      const full = join(realpathSync(abs), ...tail);
      if (full !== workspace && !full.startsWith(workspace + sep)) return null;
      const rel = relative(workspace, full);
      return rel === '' ? null : rel.split(sep).join('/');
    } catch {
      tail.unshift(basename(abs));
      const parent = dirname(abs);
      if (parent === abs) return null;
      abs = parent;
    }
  }
}

/**
 * Which side of the policy a workspace-relative path falls on, or null for neither. Anchored at
 * the worktree root, component-for-component — the same matching as the host's
 * `path_policy::under_bound_mask`, because the binds sit there: `.env` does not match `.envrc`,
 * and `.claude/` covers everything below itself but nothing nested by coincidence of spelling.
 * @param {{ masked: string[], protected: string[] }} policy  as parsePathPolicy returned
 * @param {string} relPath  workspace-relative, `/`-separated
 * @returns {'masked' | 'protected' | null}
 */
export function matchPathPolicy(policy, relPath) {
  const path = String(relPath ?? '');
  if (!path || path.startsWith('/')) return null;
  const hit = (entries) =>
    (entries ?? []).some((entry) => {
      const bare = String(entry).replace(/\/+$/, '');
      return path === bare || path.startsWith(`${bare}/`);
    });
  if (hit(policy?.masked)) return 'masked';
  if (hit(policy?.protected)) return 'protected';
  return null;
}

/**
 * The `path_policy` event one tool call warrants, or null for none: the tool must take a path,
 * the path must resolve inside the workspace, and it must land on a side of the policy that side
 * actually enforces (a read of a protected path is allowed, so it is not an attempt).
 * @param {{ masked: string[], protected: string[] } | null} policy  as loadPathPolicy returned
 * @param {string} toolName    the SDK tool name (`Read`, `Write`, `Grep`, …)
 * @param {object} [input]     the tool call's input
 * @param {{ workspace?: string }} [opts]  the workspace the tool's relative paths name
 * @returns {{ type: 'path_policy', access: 'read' | 'write', policy: 'masked' | 'protected', path: string, tool: string } | null}
 */
export function evaluatePathPolicy(policy, toolName, input, { workspace = process.cwd() } = {}) {
  if (!policy) return null;
  const spec = TOOL_PATHS.get(toolName);
  if (!spec) return null;
  const raw = input?.[spec.field];
  if (typeof raw !== 'string' || !raw) return null;
  const rel = resolveUnderWorkspace(workspace, raw);
  if (!rel) return null;
  const rule = matchPathPolicy(policy, rel);
  if (!rule || (spec.access === 'read' && rule === 'protected')) return null;
  return { type: 'path_policy', access: spec.access, policy: rule, path: rel, tool: toolName };
}

/** Reads a file up to `maxBytes`, or throws when it is longer — a policy file is not data. */
function readTextCapped(path, maxBytes) {
  const bytes = readFileSync(path);
  if (bytes.length > maxBytes) throw new Error(`${path} is over the ${maxBytes}-byte cap`);
  return bytes.toString('utf8');
}
