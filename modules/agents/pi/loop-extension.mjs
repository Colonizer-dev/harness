// Pi extension that gives a loop colony the loop tools (issue #643): loop_stop, and loop_next when
// the loop is self-paced (docs/loops.md). The runner loads it with an explicit --extension path
// (--no-extensions only stops discovery), and only for a loop colony. The tool logic is
// loop-tools.mjs, a byte-identical copy of the ACP module's file: each call crosses the runner's
// loopback loop bridge (COLONIZER_BRIDGE_URL, COLONIZER_BRIDGE_TOKEN) and leaves the colony as a
// `loop_next` or `loop_stop` protocol event, the wire the codex, grok-build and hermes modules use.

import { callLoopTool, loopSwitches, loopTools } from './loop-tools.mjs';

/** The Pi tool definitions: JSON Schema parameters (Pi validates plain JSON Schema as well as TypeBox). */
export function piLoopTools(env = process.env, { fetchImpl = fetch } = {}) {
  const bridge = { url: env.COLONIZER_BRIDGE_URL ?? '', token: env.COLONIZER_BRIDGE_TOKEN ?? '', fetchImpl };
  return loopTools(loopSwitches(env)).map((tool) => ({
    name: tool.name,
    label: tool.name,
    description: tool.description,
    parameters: tool.inputSchema,
    async execute(_toolCallId, params) {
      const answer = await callLoopTool(tool.name, params, bridge);
      if (answer.isError) throw new Error(answer.text); // Pi reports a thrown execute as a failed tool call
      return { content: [{ type: 'text', text: answer.text }], details: {} };
    },
  }));
}

export default function colonizerLoop(pi) {
  for (const tool of piLoopTools()) pi.registerTool(tool);
}
