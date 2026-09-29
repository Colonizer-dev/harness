// Pi extension that gives the agent shared memory's read tools (issue #766): memory_briefing,
// memory_changes and memory_search over the store mounted at COLONIZER_MEMORY_DIR. The runner loads
// it with an explicit --extension path (--no-extensions only stops discovery), and only when that
// dir is mounted. The tool logic is memory-mcp.mjs and memory.mjs, byte-identical copies of the
// ACP and claude-code modules' files; memory is pulled through these tools, never injected.

import { callMemoryTool, MEMORY_READ_TOOLS, memoryState } from './memory-mcp.mjs';

/** The Pi tool definitions: JSON Schema parameters (Pi validates plain JSON Schema as well as TypeBox). */
export function memoryTools(dir, state = memoryState()) {
  return MEMORY_READ_TOOLS.map((tool) => ({
    name: tool.name,
    label: tool.name,
    description: tool.description,
    parameters: tool.inputSchema,
    async execute(_toolCallId, params) {
      return { content: [{ type: 'text', text: await callMemoryTool(tool.name, params, { dir, state }) }], details: {} };
    },
  }));
}

export default function colonizerMemory(pi) {
  const dir = process.env.COLONIZER_MEMORY_DIR ?? '';
  if (!dir) return;
  for (const tool of memoryTools(dir)) pi.registerTool(tool);
}
