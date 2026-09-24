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
// lacks skills/<name>/SKILL.md) | skill-name | skill-frontmatter | skill-duplicate
// (case-insensitive, within the manifest list or within skills/) | mcp-parse | mcp-shape
// | mcp-server (needs a stdio command or remote url) | mcp-remote-hosts (a remote url needs
// non-empty hosts/allowedHosts, so the sandbox gate keeps working).
//
// Hook point (NOT yet wired; issue #370): in scripts/update-vendored-plugins.mjs main(), after
// diffSkills(...) and before applyUpdate(...), extract the `after` archive to a temp dir
// and call validatePack(topDir), skipping the pin when !ok. Plumbing the extracted tree
// through is more than a <10-line hook, so this stays standalone:
// `node scripts/validate-plugins.mjs <dir>...`.

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const err = (file, rule, message) => ({ file, rule, message });

/** Plain names only, mirroring `is_plain_name` in crates/colonizer/src/util.rs. */
const isPlainName = (name) => typeof name === 'string' && /^(?!\.)(?!.*\.\.)[^/\\:,\0]+$/.test(name);

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
