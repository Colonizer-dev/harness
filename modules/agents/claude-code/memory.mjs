// Shared memory, runner side (docs/protocol.md §6.2). Approved notes are mounted read-only under
// COLONIZER_MEMORY_DIR; the agent searches them and proposes new ones. Proposals leave the colony as
// protocol events; nothing is written inside the colony.

import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';

export const MEMORY_SCOPES = ['repo', 'org', 'global'];
export const MEMORY_SERVER = 'colonizer_memory';
export const MEMORY_PROPOSE_TOOL = `mcp__${MEMORY_SERVER}__memory_propose`;
export const MEMORY_TOOLS = [`mcp__${MEMORY_SERVER}__memory_search`, MEMORY_PROPOSE_TOOL];
export const PROPOSED_REPLY = 'Proposed for review; it becomes shared memory once approved.';

export const MEMORY_PROMPT_APPEND = [
  '- Shared memory from earlier colonies lives in /colonizer/memory/{repo,org,global}: repo is this repository, org its GitHub organisation, global everything. Read the MEMORY.md index of each before starting.',
  '- Use the memory_search tool whenever you are unsure about a convention, command or past decision.',
  '- Use memory_propose only for durable, reusable learnings (conventions, gotchas, decisions) that would help a future colony. Never propose secrets, credentials or task-specific details. Proposals are reviewed before they become shared memory. Subagents can search memory but cannot propose: ask them to include anything worth remembering in their reports, and decide yourself what to propose.',
].join('\n');

/** Why a memory_propose call is refused, or null. Subagents read shared memory; only the orchestrator proposes. */
export function memoryDecision(toolName, hookInput = {}) {
  if (toolName !== MEMORY_PROPOSE_TOOL || !hookInput.agent_id) return null;
  return 'Only the orchestrator proposes shared memory. Put this learning in your report, and the orchestrator will decide whether to propose it.';
}

const MAX_RESULTS = 10;
const SNIPPET_RADIUS = 120;

function noteTitle(text, file) {
  const heading = text.match(/^#{1,6}\s+(.+)$/m);
  if (heading) return heading[1].trim();
  const first = text.split('\n').find((line) => line.trim());
  return first ? first.trim().slice(0, 120) : file.replace(/\.md$/, '');
}

function snippet(text, term) {
  const lower = text.toLowerCase();
  const at = Math.max(0, lower.indexOf(term));
  const start = Math.max(0, at - SNIPPET_RADIUS);
  const end = Math.min(text.length, at + term.length + SNIPPET_RADIUS);
  const body = text.slice(start, end).replace(/\s+/g, ' ').trim();
  return `${start > 0 ? '…' : ''}${body}${end < text.length ? '…' : ''}`;
}

/** Case-insensitive search where every term must appear in the note. Repo notes rank first. */
export async function searchMemory(dir, query, { limit = MAX_RESULTS } = {}) {
  const terms = String(query ?? '').toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return [];
  const results = [];
  for (const scope of MEMORY_SCOPES) {
    let files;
    try {
      files = (await readdir(join(dir, scope, 'notes'))).filter((f) => f.endsWith('.md')).sort();
    } catch {
      continue; // scope not mounted
    }
    for (const file of files) {
      let text;
      try {
        text = await readFile(join(dir, scope, 'notes', file), 'utf8');
      } catch {
        continue;
      }
      const lower = text.toLowerCase();
      if (!terms.every((term) => lower.includes(term))) continue;
      results.push({ scope, title: noteTitle(text, file), file: `${scope}/notes/${file}`, snippet: snippet(text, terms[0]) });
      if (results.length >= limit) return results;
    }
  }
  return results;
}

export function formatResults(results) {
  if (!results.length) return 'No shared memory matches that query.';
  return results.map((r) => `[${r.scope}] ${r.title} (/colonizer/memory/${r.file})\n${r.snippet}`).join('\n\n');
}

/**
 * Builds the in-process MCP server with memory_search and memory_propose.
 * The SDK helpers and zod are injected so tests can run without the SDK transport.
 */
export function createMemoryServer({ dir, emit, createSdkMcpServer, tool, z }) {
  const text = (value) => ({ content: [{ type: 'text', text: value }] });
  const search = tool(
    'memory_search',
    'Search shared memory (notes approved from earlier colonies) for this repository, its GitHub organisation and globally. All terms must match; case-insensitive.',
    { query: z.string().min(1).max(500) },
    async ({ query }) => text(formatResults(await searchMemory(dir, query))),
  );
  const propose = tool(
    'memory_propose',
    'Propose a durable, reusable learning for shared memory. It is reviewed by a human before other colonies can see it. Never include secrets.',
    {
      scope: z.enum(['repo', 'org', 'global']),
      title: z.string().min(1).max(200),
      content: z.string().min(1).max(20000),
      tags: z.array(z.string().min(1).max(40)).max(10).optional(),
    },
    async ({ scope, title, content, tags }) => {
      emit({ type: 'memory_proposal', scope, title, content, tags: tags ?? [] });
      return text(PROPOSED_REPLY);
    },
  );
  return createSdkMcpServer({ name: MEMORY_SERVER, version: '1.0.0', tools: [search, propose] });
}
