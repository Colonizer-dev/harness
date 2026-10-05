// Pi extension that gives the agent shared memory's read tools (issue #766): memory_briefing,
// memory_changes and memory_search over the store mounted at COLONIZER_MEMORY_DIR. The runner loads
// it with an explicit --extension path (--no-extensions only stops discovery), and only when that
// dir is mounted. The tool logic is memory-mcp.mjs and memory.mjs, byte-identical copies of the
// ACP and claude-code modules' files; memory is pulled through these tools, never injected. When
// the operator vault is staged (COLONIZER_VAULT_DIR, issue #777) it also registers vault_search,
// from vault.mjs; Pi has no bridge to the mothership, so vault_propose is not offered here.

import { callMemoryTool, callVaultSearch, MEMORY_READ_TOOLS, memoryState, VAULT_SEARCH_TOOL } from './memory-mcp.mjs';

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

/** The operator vault's vault_search (issue #777), over the read-only snapshot at `vaultDir`. */
export function vaultTools(vaultDir) {
  return [
    {
      name: VAULT_SEARCH_TOOL.name,
      label: VAULT_SEARCH_TOOL.name,
      description: VAULT_SEARCH_TOOL.description,
      parameters: VAULT_SEARCH_TOOL.inputSchema,
      async execute(_toolCallId, params) {
        return { content: [{ type: 'text', text: await callVaultSearch(params, { vaultDir }) }], details: {} };
      },
    },
  ];
}

export default function colonizerMemory(pi) {
  const dir = process.env.COLONIZER_MEMORY_DIR ?? '';
  const vaultDir = process.env.COLONIZER_VAULT_DIR ?? '';
  if (dir) for (const tool of memoryTools(dir)) pi.registerTool(tool);
  if (vaultDir) for (const tool of vaultTools(vaultDir)) pi.registerTool(tool);
}
