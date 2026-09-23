#!/usr/bin/env node
// Reads Claude Code's built-in `general-purpose` and `Explore` agents out of its binary and checks them
// against the copies modules/agents/claude-code/subagents.mjs redefines them with (to set their effort).
// A Claude Code bump can rewrite a built-in under those copies, and a deny list the built-in grew would
// leave our Explore wider than the agent it replaces. vendor/claude-code-builtins.json is the snapshot
// for the build vendor/claude-code.lock pins; scripts/test/builtin-subagents.test.mjs fails CI while the
// lock, the snapshot and subagents.mjs disagree.
//
//   node scripts/builtin-subagents.mjs <claude>                    compare, and print the result as Markdown (exits 1 when stale)
//   node scripts/builtin-subagents.mjs --write <claude>            also rewrite the snapshot
//   node scripts/builtin-subagents.mjs --summary out.md <claude>   append the Markdown to out.md (the pin pull request's body)
//
// Only a build the lock pins is read: its sha256 has to be one of the lock's rows, and that pin is what
// trusting it rests on. The binary is a Bun standalone executable, whose JavaScript modules sit in a
// payload in front of a `---- Bun! ----` trailer. Of it, only the agent objects, the string and array
// constants they name and their prompt functions run, in a fresh node:vm context. That keeps accidental
// side effects out but is no sandbox — the stubs and errors below are objects of this realm, a way back
// to `process` — so the pin workflow also runs this with an empty environment under Node's permission
// model. Minified names change with every build, so nothing here names one: the agents are found by
// their agentType, and every other identifier by its chunk's own definition or imports. Anything else
// fails the run instead of guessing.

import { createHash } from 'node:crypto';
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, posix, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import vm from 'node:vm';

import { subagentDefinitions } from '../modules/agents/claude-code/subagents.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
export const LOCK = join(root, 'vendor/claude-code.lock');
export const SNAPSHOT = join(root, 'vendor/claude-code-builtins.json');
const SUBAGENTS = 'modules/agents/claude-code/subagents.mjs';
export const REFRESH = 'sh scripts/fetch-agent-binary.sh && node scripts/builtin-subagents.mjs --write dist/bin/claude-guest';
const AGENTS = ['general-purpose', 'Explore'];
const TRAILER = '\n---- Bun! ----\n';
const CLOSERS = { '"': '"', "'": "'", '`': '`', '[': ']' };

/** A Bun standalone executable's modules, name → contents. The 32 bytes before the trailer are the
 * header: payload size (u64), then the module table's offset and length in the payload. A table
 * record is 13 u32s: offset/length pairs for name, contents and four fields not read here, then
 * encoding, loader, format and side bytes. Source modules are Latin-1 (1); 2 is UTF-8. */
export function readBunModules(buf) {
  const header = buf.lastIndexOf(TRAILER) - 32;
  if (header < 0) throw new Error('not a Bun standalone executable: no `---- Bun! ----` trailer');
  const payload = header - Number(buf.readBigUInt64LE(header));
  const [table, tableLength] = [buf.readUInt32LE(header + 8), buf.readUInt32LE(header + 12)];
  if (payload < 0 || tableLength % 52 || payload + table + tableLength > header) {
    throw new Error('the Bun payload header is not in the layout this script knows');
  }
  const modules = new Map();
  for (let record = payload + table; record < payload + table + tableLength; record += 52) {
    const field = (n, encoding) => {
      const start = payload + buf.readUInt32LE(record + 8 * n);
      return buf.toString(encoding, start, start + buf.readUInt32LE(record + 8 * n + 4));
    };
    modules.set(field(0, 'utf8'), field(1, buf[record + 48] === 2 ? 'utf8' : 'latin1'));
  }
  return modules;
}

/** The literal starting at `start`: the first `closer` after which the text parses. */
function literalAt(source, start, closer, what) {
  for (let end = source.indexOf(closer, start + 1); end > 0 && end - start < 200_000; end = source.indexOf(closer, end + 1)) {
    const text = source.slice(start, end + 1);
    try {
      new vm.Script(`(${text})`);
      return text;
    } catch {}
  }
  throw new Error(`${what}: no end found for \`${source.slice(start, start + 60)}…\``);
}

const specifiers = (list) => list.split(',').map((spec) => spec.trim().split(/\s+as\s+/)).filter(([name]) => name);

/** `{ 'general-purpose': { description, prompt }, Explore: { description, prompt, disallowedTools } }`,
 * each prompt as its lines, from the modules of a Claude Code binary. `stubbed` collects a
 * `[name, start of its source]` pair for every helper stubbed out (see below). */
export function extractBuiltins(modules, stubbed = []) {
  const context = vm.createContext({});
  const globals = vm.runInContext('globalThis', context);
  const run = (text, scope) => vm.runInContext(`(function () { with (this) return (${text}\n) })`, context).call(scope);
  const cache = new Map();
  const memo = (key, fn) => (cache.has(key) ? cache.get(key) : cache.set(key, fn()).get(key));
  const sourceOf = (chunk) => {
    if (!modules.has(chunk)) throw new Error(`no module ${chunk} in the binary`);
    return modules.get(chunk);
  };
  const importsOf = (chunk) => memo(`imports ${chunk}`, () => new Map([...sourceOf(chunk).matchAll(/import\s*\{([^}]*)\}\s*from\s*"([^"]+)"/g)]
    .flatMap(([, list, from]) => specifiers(list).map(([name, local]) => [local ?? name, [posix.resolve(posix.dirname(chunk), from), name]]))));
  const exportsOf = (chunk) => memo(`exports ${chunk}`, () => new Map([...sourceOf(chunk).matchAll(/export\s*\{([^}]*)\}/g)]
    .flatMap(([, list]) => specifiers(list).map(([local, name]) => [name ?? local, local]))));

  // Every free identifier that is not a JavaScript global resolves through the chunk it appears in:
  // an import leads to the exporting chunk, anything else has to have exactly one definition there.
  // Symbol lookups (Symbol.unscopables) find nothing. `top` is the agent object's own scope.
  const scope = (chunk, top) => new Proxy({}, {
    has: (_, key) => typeof key === 'string' && !(key in globals),
    get: (_, key) => (typeof key === 'string' ? resolve(chunk, key, top) : undefined),
  });
  const resolve = (chunk, name, top) => memo(`${top} ${chunk} ${name}`, () => {
    const imported = importsOf(chunk).get(name);
    if (imported) return resolve(imported[0], exportsOf(imported[0]).get(imported[1]) ?? imported[1], top);
    const source = sourceOf(chunk);
    const id = name.replace(/\$/g, '\\$');
    const found = [...source.matchAll(new RegExp(`(?:(?<![\\w$.])(?:var|let|const) |,)${id}=(?![=>])|(?<![\\w$.])function ${id}\\(`, 'g'))];
    if (found.length !== 1) throw new Error(`${name}: ${found.length ? `${found.length} candidate definitions` : 'no definition'} and no import in ${chunk}`);
    const [match] = found;
    if (match[0].startsWith('function')) {
      const text = literalAt(source, match.index, '}', name);
      if (top) return run(text, scope(chunk, false));
      // Only the functions an agent object names run: its prompt function. The helpers that one calls
      // are environment checks, each stubbed to answer true, which in 2.1.280 renders the prompt a
      // colony gets: a POSIX guest whose Bash has the embedded find and grep (runner.mjs opts into
      // neither the Glob nor the Grep tool). A later helper whose true means something else would pick
      // another branch silently — the stub throws only when its value is used as text or an object —
      // so a changed prompt is shown with the stubbed helpers listed, for a person to check.
      stubbed.push([name, text.length > 100 ? `${text.slice(0, 100)}…` : text]);
      return () => new Proxy({}, { get: () => { throw new Error(`${name}() is stubbed as a condition, but the prompt uses its value`); } });
    }
    const start = match.index + match[0].length;
    if (!CLOSERS[source[start]]) throw new Error(`${name} in ${chunk} is \`${source.slice(start, start + 40)}…\`, not a string or an array`);
    const text = literalAt(source, start, CLOSERS[source[start]], name);
    if (!/^(?:[,;\n]|$)/.test(source.slice(start + text.length, start + text.length + 1))) {
      throw new Error(`${name} in ${chunk} is more than a literal: \`${source.slice(start, start + text.length + 20)}…\``);
    }
    return run(text, scope(chunk, false));
  });

  return Object.fromEntries(AGENTS.map((agentType) => {
    const needle = `{agentType:${JSON.stringify(agentType)},`;
    const found = [];
    for (const [chunk, source] of modules) {
      for (let at = source.indexOf(needle); at >= 0; at = source.indexOf(needle, at + 1)) {
        const text = literalAt(source, at, '}', agentType);
        if (text.includes('getSystemPrompt:')) found.push({ chunk, text });
      }
    }
    if (found.length !== 1) throw new Error(`${found.length} built-in ${agentType} agents (\`${needle}…getSystemPrompt:…}\`) in the binary, not one`);
    const agent = run(found[0].text, scope(found[0].chunk, true));
    const prompt = agent.getSystemPrompt();
    const denied = agent.disallowedTools;
    if (typeof agent.whenToUse !== 'string' || typeof prompt !== 'string' ||
      (denied !== undefined && !(Array.isArray(denied) && denied.every((tool) => typeof tool === 'string')))) {
      throw new Error(`the built-in ${agentType} agent does not have the shape this script knows`);
    }
    // Copied out of the vm's realm, so they compare equal to ordinary arrays.
    return [agentType, { description: agent.whenToUse, prompt: prompt.split('\n'), ...(denied && { disallowedTools: [...denied] }) }];
  }));
}

/** Our copies, in the snapshot's shape. */
export function ours() {
  const definitions = subagentDefinitions('medium');
  return Object.fromEntries(AGENTS.map((agent) => {
    const { description, prompt, disallowedTools } = definitions[agent];
    return [agent, { description, prompt: prompt.split('\n'), ...(disallowedTools && { disallowedTools }) }];
  }));
}

/** A line diff from `a` to `b` (LCS): `-` only in `a`, `+` only in `b`, ` ` in both. */
function diffLines(a, b) {
  const lcs = Array.from({ length: a.length + 1 }, () => new Array(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) for (let j = b.length - 1; j >= 0; j--) {
    lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
  }
  const out = [];
  for (let i = 0, j = 0; i < a.length || j < b.length;) {
    if (i < a.length && j < b.length && a[i] === b[j]) out.push(` ${a[i++]}`), j++;
    else if (i < a.length && (j === b.length || lcs[i + 1][j] >= lcs[i][j + 1])) out.push(`-${a[i++]}`);
    else out.push(`+${b[j++]}`);
  }
  return out;
}

/** What makes our copies stale, per agent: `missing`, tools the built-in denies and ours does not (the
 * other way round is fine: subagents.mjs also denies Task), and `diff`, a description or prompt that
 * differs, as lines from ours to the built-in's. */
export function compare(builtins, copies) {
  return AGENTS.flatMap((agent) => {
    const mine = copies[agent].disallowedTools ?? [];
    const missing = (builtins[agent].disallowedTools ?? []).filter((tool) => !mine.includes(tool));
    return [
      ...(missing.length ? [{ agent, missing }] : []),
      ...['description', 'prompt'].flatMap((field) => {
        const diff = diffLines([copies[agent][field]].flat(), [builtins[agent][field]].flat());
        return diff.some((line) => line[0] !== ' ') ? [{ agent, field, diff }] : [];
      }),
    ];
  });
}

/** The comparison as a Markdown section, the shape scripts/update-runtime-pins.mjs gives its own. */
export function markdown(version, differences, stubbed = []) {
  const fenced = (lang, lines) => {
    const fence = '`'.repeat(Math.max(3, ...(lines.join('\n').match(/`+/g) ?? []).map((run) => run.length + 1)));
    return [`${fence}${lang}`, ...lines, fence];
  };
  const lines = ['### Built-in subagents', '', differences.length
    ? `Claude Code ${version}'s built-in agents differ from the copies \`${SUBAGENTS}\` redefines them with (to set their effort). Refresh those copies to match \`vendor/claude-code-builtins.json\` before this bump merges, or with it in a pull request of your own that includes this commit — not on this branch: the daily run rebuilds it from main, so commits pushed to it are lost. CI fails until the copies match.`
    : `Claude Code ${version}'s built-in \`general-purpose\` and \`Explore\` agents match the copies in \`${SUBAGENTS}\`.`];
  // The deny lists first: those are the differences that widen what a colony's subagent can do.
  for (const d of [...differences.filter((d) => d.missing), ...differences.filter((d) => d.diff)]) {
    if (d.missing) lines.push('', `**The built-in \`${d.agent}\` denies ${d.missing.map((tool) => `\`${tool}\``).join(', ')}, and ours does not**: until ours does too, a colony's \`${d.agent}\` can use tools the built-in it replaces cannot.`);
    else lines.push('', `\`${d.agent}\` ${d.field}, ours → the built-in's:`, '', ...fenced('diff', d.diff));
  }
  if (stubbed.length && differences.some((d) => d.field === 'prompt')) {
    lines.push('', 'The built-in prompts were rendered with these helpers stubbed to answer true. Check that the diff shows the branch a colony gets (a POSIX guest, `find` and `grep` via Bash):', '',
      ...fenced('js', stubbed.map(([name, source]) => `${name}: ${source.replace(/\n/g, ' ')}`)));
  }
  return `${lines.join('\n')}\n`;
}

/** The lock's rows: `{ name, version, platform, kind, sha, url }`. */
export function lockRows(path) {
  return readFileSync(path, 'utf8').split('\n').filter((line) => line.trim() && !line.trimStart().startsWith('#'))
    .map((line) => line.trim().split(/\s+/)).map(([name, version, platform, kind, sha, url]) => ({ name, version, platform, kind, sha, url }));
}

function main() {
  const args = process.argv.slice(2);
  const summary = args.includes('--summary') ? args[args.indexOf('--summary') + 1] : undefined;
  const [binary] = args.filter((arg, i) => !arg.startsWith('--') && args[i - 1] !== '--summary');
  if (!binary) {
    console.error('usage: node scripts/builtin-subagents.mjs [--write] [--summary out.md] <claude-binary>');
    process.exit(2);
  }
  try {
    const buf = readFileSync(binary);
    const sha = createHash('sha256').update(buf).digest('hex');
    const row = lockRows(LOCK).find((r) => r.kind === 'agent' && r.sha === sha);
    if (!row) throw new Error(`${binary} (sha256 ${sha}) is not a build vendor/claude-code.lock pins; fetch one with sh scripts/fetch-agent-binary.sh`);
    const stubbed = [];
    const builtins = extractBuiltins(readBunModules(buf), stubbed);
    const differences = compare(builtins, ours());
    if (args.includes('--write')) {
      writeFileSync(SNAPSHOT, `${JSON.stringify({ version: row.version, ...builtins }, null, 2)}\n`);
      console.error(`wrote ${relative(process.cwd(), SNAPSHOT)} (${row.platform})`);
    }
    const section = markdown(row.version, differences, stubbed);
    process.stdout.write(section);
    if (summary) appendFileSync(summary, `\n\n${section}`);
    if (differences.length) process.exitCode = 1;
  } catch (error) {
    console.error(error.message);
    if (summary) {
      appendFileSync(summary, `\n\n### Built-in subagents\n\nThe built-in agents could not be read out of this Claude Code build (${error.message}), so review the copies in \`${SUBAGENTS}\` against it by hand; CI fails until \`vendor/claude-code-builtins.json\` is at the new version.\n`);
    }
    process.exit(1);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) main();
