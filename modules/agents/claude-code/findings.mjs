// Findings, runner side (docs/protocol.md §6.6). Something noticed outside the colony's task leaves
// the colony as a protocol event; the mothership files it as a GitHub issue, because the GitHub
// token never enters a colony. Only the orchestrator may file, and only with the evidence of how the
// finding was confirmed.

export const FINDINGS_SERVER = 'colonizer_findings';
export const FINDING_TOOL = `mcp__${FINDINGS_SERVER}__finding_file`;
export const FILED_REPLY =
  'Sent to the harness. It files this as a GitHub issue on the repository unless an open issue already has the same title; the outcome appears in the colony log, not here.';

export const FINDINGS_PROMPT_APPEND = [
  '- When you or a subagent notice a real problem outside this task — a bug, a security gap, documentation that promises what the code does not do — do not fix it in this pull request. File it with the finding_file tool so it is tracked.',
  '- Before filing, have a fresh subagent confirm the finding independently against the code, and file only what it confirms. Put what was checked, and how, in the evidence field. Subagents cannot file findings: ask them to include anything out of scope in their reports, and decide yourself what is worth filing.',
  '- File only findings a maintainer would act on, each one self-contained. Never file what this task already fixes, style preferences, or anything containing secrets. One colony may file at most five.',
].join('\n');

/** Why a finding_file call is refused, or null. Subagents report findings; the orchestrator files them. */
export function findingDecision(toolName, hookInput = {}) {
  if (toolName !== FINDING_TOOL || !hookInput.agent_id) return null;
  return 'Only the orchestrator files findings. Put this finding in your report, with how you confirmed it, and the orchestrator will decide whether to file it.';
}

/**
 * Builds the in-process MCP server with finding_file.
 * The SDK helpers and zod are injected so tests can run without the SDK transport.
 */
export function createFindingsServer({ emit, createSdkMcpServer, tool, z }) {
  const file = tool(
    'finding_file',
    'File a confirmed problem found outside this task as a GitHub issue on this repository. Orchestrator only; confirm it with a subagent first.',
    {
      title: z.string().min(1).max(200).describe('One line, specific enough to find later: what is wrong, and where'),
      body: z.string().min(1).max(20000).describe('Markdown: what is wrong, where (files and lines), why it matters, and a suggested fix if there is one'),
      evidence: z.string().min(1).max(5000).describe('How the finding was confirmed: which subagent checked it, what it read or ran, and what it found'),
    },
    async ({ title, body, evidence }) => {
      emit({ type: 'finding', title, body, evidence });
      return { content: [{ type: 'text', text: FILED_REPLY }] };
    },
  );
  return createSdkMcpServer({ name: FINDINGS_SERVER, version: '1.0.0', tools: [file] });
}
