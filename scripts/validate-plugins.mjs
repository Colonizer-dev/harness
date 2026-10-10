#!/usr/bin/env node
// Validates a skill-pack dir against the canonical Agent Plugins layout (issue #294):
// `plugin.json` at the pack root ({name, version, description, skills}), optional `mcp.json`,
// and `skills/<name>/SKILL.md` files with `name:`/`description:` frontmatter.
//
// Each failure is {file, rule, message}. Rules:
// manifest-missing (neither plugin.json nor legacy .claude-plugin/plugin.json exists; root
// wins when both exist) | manifest-parse | manifest-shape (valid JSON but not an object) |
// manifest-schema ($schema must be https:// or absent) | manifest-name (plain name a la is_plain_name in plugins.rs; NOT required to
// match the dir basename, so temp/staging checkouts validate) | manifest-version (semver)
// | manifest-description | manifest-skills (must be an array) | skill-missing (listed skill
// lacks skills/<name>/SKILL.md) | skill-name (a legacy directory-style entry like `./skills/`
// is accepted, mirroring is_skill_tree in plugins.rs) | skill-frontmatter | skill-duplicate
// (case-insensitive, within the manifest list or within skills/) | mcp-parse | mcp-shape
// | mcp-server (needs a stdio command or remote url) | mcp-remote-hosts (a remote url needs
// non-empty hosts/allowedHosts, so the sandbox gate keeps working).
//
// Agents (issue #1163): every agents/*.md is a Claude Code agent file — frontmatter plus a system
// prompt — read with a small YAML subset that mirrors parse_agent_frontmatter in
// crates/colonizer/src/plugins.rs exactly: `key: value` scalars bare or double-quoted, inline
// arrays `[a, b]` and inline maps `{ k: "v" }`, and one nested block level at exactly two spaces
// of indent, which only `ant:` opens. Plain Claude Code frontmatter passes unchanged (unknown
// top-level keys are Claude Code's to add); Colonizer's own fields are judged when present.
// Rules: agent-frontmatter (a leading and closing --- block, a non-empty name and description,
// unparseable lines named by their number, non-empty tools/disallowedTools entries) | agent-name
// (the name is a plain name a la is_plain_name) | agent-caste (the ant block's display_name and
// caste — forager, soldier, weaver, honeypot, scout, worker or queen — and no unknown ant keys)
// | agent-colors (exactly body/dark/accent, each a double-quoted #rrggbb string) | agent-skillsets
// (each entry a plain name) | agent-duplicate (one pack cannot define the same agent name twice;
// the cross-pack duplicate check runs at boot, plugins.rs check_ant_uniqueness).
//
// Wired in at every gate (issue #370): the vendored-plugin updater runs validatePack on the new
// archive of each pin it stages from that archive and skips the pin when !ok
// (scripts/update-vendored-plugins.mjs); CI and the updater's proposal workflow stage the pinned
// packs (VENDOR_KINDS="plugin prompt" sh scripts/fetch-vendor.sh) and run this CLI over
// dist/plugins/*; and the Rust boot path (crates/colonizer/src/plugins.rs) enforces the mcp-server
// and mcp-remote-hosts rules with the same accepted shapes. Standalone use:
// `node scripts/validate-plugins.mjs <dir>...`.

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const err = (file, rule, message) => ({ file, rule, message });

/** Plain names only, mirroring `is_plain_name` in crates/colonizer/src/util.rs. */
const isPlainName = (name) => typeof name === 'string' && /^(?!\.)(?!.*\.\.)[^/\\:,\0]+$/.test(name);

const isFileAt = (path) => {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
};

const isDirectory = (path) => {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
};

/** Directory entries of `dir` as paths, following symlinks (a linked directory counts, an
 * unreadable or dangling one is skipped) — the semantics of `is_dir()` on the path in Rust,
 * not the lstat a Dirent carries. */
const subdirectories = (dir) => {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return [];
  }
  const dirs = [];
  for (const entry of entries) {
    const path = join(dir, entry.name);
    try {
      if (statSync(path).isDirectory()) dirs.push(path);
    } catch {
      // Dangling symlink or unreadable: not a directory we can walk.
    }
  }
  return dirs;
};

/** A legacy directory-style manifest entry (`"./skills/"`, `"skills/"`, `"."`): "every skill under
 * that directory", the shape upstream ecc ships. Accepted exactly as `is_skill_tree` in
 * crates/colonizer/src/plugins.rs accepts it: `rel` (leading `./` and slashes already stripped,
 * never containing `..`) resolves under `dir` to a directory holding a `SKILL.md` at most two
 * levels beneath it. */
function isSkillTree(dir, rel) {
  if (rel.includes('..')) return false;
  const base = rel === '.' ? dir : join(dir, rel);
  if (!isDirectory(base)) return false;
  const stack = [[base, 0]];
  while (stack.length > 0) {
    const [sub, depth] = stack.pop();
    if (isFileAt(join(sub, 'SKILL.md'))) return true;
    if (depth >= 2) continue;
    for (const child of subdirectories(sub)) stack.push([child, depth + 1]);
  }
  return false;
}

const SEMVER = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/;

/** The leading `---` block as {key: value}, or null when there is none. */
function parseFrontmatter(text) {
  const lines = text.split('\n');
  if (lines[0].trim() !== '---') return null;
  const end = lines.findIndex((line, i) => i > 0 && line.trim() === '---');
  if (end < 0) return null;
  const fields = {};
  for (const line of lines.slice(1, end)) {
    const match = /^([A-Za-z0-9_-]+)\s*:\s*(.*)$/.exec(line);
    if (match) fields[match[1]] = match[2].trim().replace(/^['"]|['"]$/g, '');
  }
  return fields;
}

/** The server map inside an mcp.json doc: `mcpServers`/`servers`, else the doc itself. */
function serverMap(doc) {
  if (!doc || typeof doc !== 'object' || Array.isArray(doc)) return null;
  for (const key of ['mcpServers', 'servers']) {
    if (doc[key] && typeof doc[key] === 'object' && !Array.isArray(doc[key])) return doc[key];
  }
  return doc;
}

// ---- Agent files (issue #1163) ----
// The YAML subset below mirrors plugins.rs's parse_agent_frontmatter line for line, so a pack
// fails the same rules in CI and at boot.

/** The castes an ant can hold. */
const CASTES = ['forager', 'soldier', 'weaver', 'honeypot', 'scout', 'worker', 'queen'];

/** The keys an `ant:` block may carry; anything else is a typo'd identity, refused. */
const ANT_KEYS = ['display_name', 'caste', 'title', 'colors', 'move'];

/** `key: value` split at the first colon; the key is letters/digits/_/-. */
const keyValue = (line) => {
  const colon = line.indexOf(':');
  if (colon < 0) return null;
  const key = line.slice(0, colon).trim();
  if (!/^[A-Za-z0-9_-]+$/.test(key)) return null;
  return [key, line.slice(colon + 1).trim()];
};

/** One scalar: double-quoted or bare. `quoted` matters only to ant.colors, whose entries must be
 * quoted so a bare `#ff0000` — which YAML would read as a comment — is refused instead. */
const scalar = (raw) => {
  const value = raw.trim();
  if (value.startsWith('"')) {
    if (value.length < 2 || !value.endsWith('"')) return null;
    return { value: value.slice(1, -1), quoted: true };
  }
  return { value, quoted: false };
};

/** Splits on commas outside double quotes, [...] and {...}, so a nested `{ k: [a, b] }` stays one
 * item for its consumer to reject. */
const splitItems = (raw) => {
  const items = [];
  let current = '';
  let quoted = false;
  let depth = 0;
  for (const ch of raw) {
    if (quoted) {
      if (ch === '"') quoted = false;
    } else if (ch === '"') {
      quoted = true;
    } else if (ch === '[' || ch === '{') {
      depth += 1;
    } else if (ch === ']' || ch === '}') {
      depth -= 1;
    } else if (ch === ',' && depth === 0) {
      items.push(current);
      current = '';
      continue;
    }
    current += ch;
  }
  items.push(current);
  return items;
};

/** An inline array `[a, b]` of scalars; [] is the empty array. */
const inlineArray = (raw) => {
  if (!raw.startsWith('[') || !raw.endsWith(']')) return null;
  const inner = raw.slice(1, -1);
  if (inner.trim() === '') return [];
  const items = [];
  for (const item of splitItems(inner)) {
    const parsed = scalar(item);
    if (!parsed) return null;
    items.push(parsed);
  }
  return items;
};

/** An inline map `{ k: "v" }` of keys to scalars. */
const inlineMap = (raw) => {
  if (!raw.startsWith('{') || !raw.endsWith('}')) return null;
  const inner = raw.slice(1, -1);
  if (inner.trim() === '') return [];
  const entries = [];
  for (const item of splitItems(inner)) {
    const pair = keyValue(item.trim());
    if (!pair) return null;
    const value = scalar(pair[1]);
    if (!value) return null;
    entries.push([pair[0], value]);
  }
  return entries;
};

/** The agent-file frontmatter under the shared YAML subset: {errors, scalars, arrays, ant}.
 * Blank lines are skipped; any other line this cannot read is an agent-frontmatter error naming
 * the line's number. */
function parseAgentFrontmatter(file, text) {
  const errors = [];
  const cannot = (number, line) =>
    errors.push(err(file, 'agent-frontmatter', `line ${number}: cannot parse ${JSON.stringify(line)}`));
  const scalars = {};
  const arrays = {};
  let ant = null;
  const lines = text.split('\n');
  if ((lines[0] ?? '').trim() !== '---') {
    errors.push(err(file, 'agent-frontmatter', 'needs a leading --- frontmatter block'));
    return { errors, scalars, arrays, ant };
  }
  const end = lines.findIndex((line, i) => i > 0 && line.trim() === '---');
  if (end < 0) {
    errors.push(err(file, 'agent-frontmatter', 'the frontmatter is missing its closing ---'));
    return { errors, scalars, arrays, ant };
  }
  for (let i = 1; i < end; i++) {
    const line = lines[i];
    const number = i + 1; // 1-based, counting the opening ---.
    if (line.trim() === '') continue;
    // Indented lines belong to the `ant:` block, at exactly two spaces and nothing else.
    if (line.startsWith(' ') || line.startsWith('\t')) {
      if (!line.startsWith('  ') || line.startsWith('   ')) {
        errors.push(err(file, 'agent-frontmatter', `line ${number}: the ant: block is indented exactly two spaces`));
        continue;
      }
      if (!ant) {
        errors.push(err(file, 'agent-frontmatter', `line ${number}: only ant: opens an indented block`));
        continue;
      }
      const pair = keyValue(line.slice(2));
      if (!pair) {
        cannot(number, line);
        continue;
      }
      if (pair[1] === '') {
        ant.set(pair[0], { value: '', quoted: false });
        continue;
      }
      const value = scalar(pair[1]);
      if (!value) {
        cannot(number, line);
        continue;
      }
      ant.set(pair[0], value);
      continue;
    }
    const pair = keyValue(line);
    if (!pair) {
      cannot(number, line);
      continue;
    }
    const [key, raw] = pair;
    if (raw === '') {
      // `ant:` opens the nested block; any other bare `key:` is an empty scalar, which the
      // required-field checks refuse where emptiness matters.
      if (key === 'ant') ant = new Map();
      else scalars[key] = { value: '', quoted: false };
      continue;
    }
    if (key === 'ant') {
      // An inline `ant: { display_name: "Sarge", caste: forager }` reads like the block form;
      // anything else spelled after `ant:` cannot be one.
      const entries = inlineMap(raw);
      if (!entries) {
        errors.push(
          err(file, 'agent-frontmatter', `line ${number}: ant: must be a block indented two spaces or an inline map`),
        );
        continue;
      }
      ant = new Map(entries);
      continue;
    }
    if (raw.startsWith('[')) {
      const items = inlineArray(raw);
      if (!items) {
        cannot(number, line);
        continue;
      }
      arrays[key] = items;
      continue;
    }
    const value = scalar(raw);
    if (!value) {
      cannot(number, line);
      continue;
    }
    scalars[key] = value;
  }
  return { errors, scalars, arrays, ant };
}

export function validatePack(dir) {
  const errors = [];
  const at = (rel) => join(dir, rel);
  const isFile = (rel) => {
    try {
      return statSync(at(rel)).isFile();
    } catch {
      return false;
    }
  };

  // Manifest: canonical root first, legacy fallback.
  const manifestFile = existsSync(at('plugin.json'))
    ? 'plugin.json'
    : existsSync(at('.claude-plugin/plugin.json'))
      ? '.claude-plugin/plugin.json'
      : null;
  if (!manifestFile) {
    errors.push(err('plugin.json', 'manifest-missing', 'neither plugin.json nor .claude-plugin/plugin.json exists'));
  }
  let manifest = null;
  if (manifestFile) {
    let parsed;
    try {
      parsed = JSON.parse(readFileSync(at(manifestFile), 'utf8'));
    } catch (e) {
      errors.push(err(manifestFile, 'manifest-parse', `not valid JSON: ${e.message}`));
    }
    if (parsed !== undefined && (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed))) {
      // A primitive or array parses fine, but `'x' in manifest` below would throw on it.
      errors.push(err(manifestFile, 'manifest-shape', 'must be a JSON object with name, version, description and skills'));
    } else if (parsed !== undefined) {
      manifest = parsed;
      if ('$schema' in manifest && (typeof manifest.$schema !== 'string' || !manifest.$schema.startsWith('https://'))) {
        errors.push(err(manifestFile, 'manifest-schema', '$schema must be an https:// URL string or absent'));
      }
      if (!isPlainName(manifest.name)) {
        errors.push(err(manifestFile, 'manifest-name', 'name must be a plain, filesystem-safe name'));
      }
      if (typeof manifest.version !== 'string' || !SEMVER.test(manifest.version)) {
        errors.push(err(manifestFile, 'manifest-version', 'version must be semver (e.g. 1.2.3)'));
      }
      if (typeof manifest.description !== 'string' || manifest.description.trim() === '') {
        errors.push(err(manifestFile, 'manifest-description', 'description must be a non-empty string'));
      }
      if ('skills' in manifest && !Array.isArray(manifest.skills)) {
        errors.push(err(manifestFile, 'manifest-skills', 'skills must be an array of skill names'));
      }
    }
  }

  // Skills: manifest-listed names must exist; on-disk SKILL.md files need frontmatter.
  const listed = Array.isArray(manifest?.skills) ? manifest.skills : [];
  // Duplicates are tracked per source: a manifest entry matching its on-disk directory
  // is the normal case, not a duplicate; two names equal case-insensitively within the
  // manifest list, or within the skills directory, collide on case-insensitive checkouts.
  const claimed = (seen, name) => {
    const key = name.toLowerCase();
    if (seen.has(key)) return true;
    seen.add(key);
    return false;
  };
  const listedNames = new Set();
  const onDiskNames = new Set();
  for (const entry of listed) {
    const name = typeof entry === 'string' ? entry : entry?.name;
    // A legacy directory-style entry means "every skill under that directory", accepted exactly
    // as plugins.rs's is_skill_tree accepts it (same normalization: trim, strip a leading `./`
    // and slashes), so both checkers agree on the packs that actually ship.
    const rel = typeof name === 'string' ? name.trim().replace(/^(\.\/)+/, '').replace(/^\/+|\/+$/g, '') : '';
    if (rel !== '' && isSkillTree(dir, rel)) continue;
    if (!isPlainName(name)) {
      errors.push(err('plugin.json', 'skill-name', `listed skill ${JSON.stringify(entry)} is not a plain, filesystem-safe name`));
      continue;
    }
    if (claimed(listedNames, name)) {
      errors.push(err('plugin.json', 'skill-duplicate', `duplicate skill name ${JSON.stringify(name)} (case-insensitive)`));
    }
    if (!isFile(`skills/${name}/SKILL.md`)) {
      errors.push(err(`skills/${name}/SKILL.md`, 'skill-missing', `listed skill ${JSON.stringify(name)} has no SKILL.md`));
    }
  }
  let onDisk = [];
  try {
    onDisk = readdirSync(at('skills'), { withFileTypes: true }).filter((e) => e.isDirectory());
  } catch {
    onDisk = [];
  }
  for (const entry of onDisk) {
    if (!isPlainName(entry.name)) {
      errors.push(err(`skills/${entry.name}`, 'skill-name', 'skill directory is not a plain, filesystem-safe name'));
      continue;
    }
    if (claimed(onDiskNames, entry.name)) {
      errors.push(err(`skills/${entry.name}/SKILL.md`, 'skill-duplicate', `duplicate skill name ${JSON.stringify(entry.name)} (case-insensitive)`));
      continue;
    }
    if (!isFile(`skills/${entry.name}/SKILL.md`)) continue;
    const fields = parseFrontmatter(readFileSync(at(`skills/${entry.name}/SKILL.md`), 'utf8'));
    for (const key of ['name', 'description']) {
      if (!fields || !fields[key]) {
        errors.push(
          err(`skills/${entry.name}/SKILL.md`, 'skill-frontmatter', `SKILL.md needs a leading --- block with non-empty ${key}:`),
        );
      }
    }
  }

  // Agents (issue #1163): every agents/*.md is an ant file — Claude Code frontmatter plus, when
  // present, Colonizer's `ant:` identity. One pack cannot define the same agent name twice (case
  // matters to nobody's eyes: compared like skill names); the cross-pack duplicate check runs at
  // boot (check_ant_uniqueness in plugins.rs).
  let agentFiles = [];
  try {
    agentFiles = readdirSync(at('agents'), { withFileTypes: true })
      .map((e) => e.name)
      .filter((name) => name.endsWith('.md') && isFileAt(at(`agents/${name}`)))
      .sort();
  } catch {
    agentFiles = [];
  }
  const agentNames = new Map();
  for (const name of agentFiles) {
    const rel = `agents/${name}`;
    const { errors: parseErrors, scalars, arrays, ant } = parseAgentFrontmatter(rel, readFileSync(at(rel), 'utf8'));
    errors.push(...parseErrors);
    const agentName = scalars.name?.value;
    if (!agentName || agentName.trim() === '') {
      errors.push(err(rel, 'agent-frontmatter', 'needs a non-empty name:'));
    } else if (!isPlainName(agentName)) {
      errors.push(err(rel, 'agent-name', `name ${JSON.stringify(agentName)} is not a plain, filesystem-safe name`));
    }
    if (!scalars.description || scalars.description.value.trim() === '') {
      errors.push(err(rel, 'agent-frontmatter', 'needs a non-empty description:'));
    }
    for (const key of ['tools', 'disallowedTools']) {
      if (arrays[key]?.some((item) => item.value.trim() === '')) {
        errors.push(err(rel, 'agent-frontmatter', `${key}: entries must be non-empty strings`));
      }
    }
    if (arrays.skillsets) {
      for (const item of arrays.skillsets) {
        if (!isPlainName(item.value)) {
          errors.push(err(rel, 'agent-skillsets', `skillset ${JSON.stringify(item.value)} is not a plain, filesystem-safe name`));
        }
      }
    }
    for (const key of ['model', 'effort']) {
      if (arrays[key]) {
        errors.push(err(rel, 'agent-frontmatter', `${key}: must be a string`));
      }
    }
    if (ant) {
      for (const key of ant.keys()) {
        if (!ANT_KEYS.includes(key)) {
          errors.push(err(rel, 'agent-caste', `unknown ant field ${JSON.stringify(key)} (known: ${ANT_KEYS.join(', ')})`));
        }
      }
      const displayName = ant.get('display_name');
      if (!displayName || displayName.value.trim() === '') {
        errors.push(err(rel, 'agent-caste', 'the ant block needs a non-empty display_name:'));
      }
      const caste = ant.get('caste');
      if (!caste || caste.value === '') {
        errors.push(err(rel, 'agent-caste', `the ant block needs a caste: one of ${CASTES.join(', ')}`));
      } else if (!CASTES.includes(caste.value)) {
        errors.push(err(rel, 'agent-caste', `unknown ant caste ${JSON.stringify(caste.value)}`));
      }
      for (const key of ['title', 'move']) {
        const field = ant.get(key);
        if (field && field.value.trim() === '') {
          errors.push(err(rel, 'agent-caste', `ant.${key} must be a non-empty string`));
        }
      }
      const colors = ant.get('colors');
      if (colors) {
        const entries = inlineMap(colors.value);
        if (!entries) {
          errors.push(
            err(rel, 'agent-colors', 'ant.colors must be an inline map { body: "#rrggbb", dark: "#rrggbb", accent: "#rrggbb" }'),
          );
        } else {
          const seen = new Set();
          for (const [key, field] of entries) {
            if (!['body', 'dark', 'accent'].includes(key)) {
              errors.push(err(rel, 'agent-colors', `unknown ant color ${JSON.stringify(key)} (known: body, dark, accent)`));
              continue;
            }
            if (!field.quoted || !/^#[0-9a-fA-F]{6}$/.test(field.value)) {
              errors.push(err(rel, 'agent-colors', `ant.colors.${key} must be a double-quoted #rrggbb hex string`));
            }
            seen.add(key);
          }
          for (const key of ['body', 'dark', 'accent']) {
            if (!seen.has(key)) {
              errors.push(err(rel, 'agent-colors', `ant.colors must list ${key}: (body, dark, accent)`));
            }
          }
        }
      }
    }
    const key = agentName?.toLowerCase();
    if (key) {
      if (agentNames.has(key)) {
        errors.push(err(rel, 'agent-duplicate', `duplicate agent name ${JSON.stringify(agentName)} (also agents/${agentNames.get(key)})`));
      } else {
        agentNames.set(key, name);
      }
    }
  }

  // mcp.json: optional; stdio entries need `command`, remote entries need hosts.
  if (existsSync(at('mcp.json'))) {
    let doc;
    try {
      doc = JSON.parse(readFileSync(at('mcp.json'), 'utf8'));
    } catch (e) {
      errors.push(err('mcp.json', 'mcp-parse', `not valid JSON: ${e.message}`));
    }
    if (doc !== undefined) {
      const servers = serverMap(doc);
      if (!servers) {
        errors.push(err('mcp.json', 'mcp-shape', 'must be an object of server entries (or one under mcpServers/servers)'));
      } else {
        for (const [server, config] of Object.entries(servers)) {
          if (!config || typeof config !== 'object' || Array.isArray(config)) {
            errors.push(err('mcp.json', 'mcp-server', `server ${JSON.stringify(server)} must be an object`));
          } else if (typeof config.command === 'string' && config.command !== '') {
            continue; // Local stdio server.
          } else if (typeof config.url === 'string' && config.url !== '') {
            const hosts = config.hosts ?? config.allowedHosts;
            const declared = Array.isArray(hosts) ? hosts.filter((h) => typeof h === 'string' && h !== '') : hosts;
            if ((Array.isArray(declared) && declared.length > 0) || (typeof declared === 'string' && declared !== '')) continue;
            errors.push(err('mcp.json', 'mcp-remote-hosts', `remote server ${JSON.stringify(server)} needs a non-empty hosts/allowedHosts declaration`));
          } else {
            errors.push(err('mcp.json', 'mcp-server', `server ${JSON.stringify(server)} needs a stdio command or a remote url`));
          }
        }
      }
    }
  }

  return { ok: errors.length === 0, errors };
}

const invoked = process.argv[1] && import.meta.url.endsWith(encodeURI(process.argv[1].split('/').pop()));
if (invoked) {
  const dirs = process.argv.slice(2).filter((a) => a !== '--help' && a !== '-h');
  if (dirs.length === 0) {
    console.error('usage: node scripts/validate-plugins.mjs <dir>...');
    process.exit(2);
  }
  let failed = 0;
  for (const dir of dirs) {
    let result;
    try {
      result = validatePack(dir);
    } catch (e) {
      console.log(`${dir}: FAIL\n  - [unreadable-dir] ${e.message}`);
      failed++;
      continue;
    }
    if (result.ok) {
      console.log(`${dir}: ok`);
    } else {
      console.log(`${dir}: FAIL`);
      for (const e of result.errors) console.log(`  ${e.file} [${e.rule}] ${e.message}`);
      failed++;
    }
  }
  process.exit(failed === 0 ? 0 : 1);
}
