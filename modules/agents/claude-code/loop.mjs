// Loops, runner side (docs/loops.md). A colony a loop launched can end its loop with loop_stop, and a
// self-paced loop's colony names its next run with loop_next. Both leave the colony as protocol
// events; the mothership owns the schedule. Only the orchestrator paces or stops the loop.

export const LOOP_SERVER = 'colonizer_loop';
export const LOOP_NEXT_TOOL = `mcp__${LOOP_SERVER}__loop_next`;
export const LOOP_STOP_TOOL = `mcp__${LOOP_SERVER}__loop_stop`;
export const NEXT_MIN_MINUTES = 15;
export const NEXT_MAX_MINUTES = 24 * 60;

export function loopPromptAppend(selfPaced) {
  return [
    '- This colony is one run of a loop: a task the operator scheduled to repeat.',
    selfPaced
      ? `- Before you finish, call loop_next with how many minutes from now the next run should start (${NEXT_MIN_MINUTES}–${NEXT_MAX_MINUTES}) and why — sooner when work is pending, later when nothing is happening.`
      : '- The loop runs on a fixed schedule; you do not schedule the next run.',
    "- When the loop's goal is met, or it should not run again, call loop_stop with the reason.",
  ].join('\n');
}

/** Why a loop tool call is refused, or null. Subagents report; the orchestrator paces the loop. */
export function loopDecision(toolName, hookInput = {}) {
  if (toolName !== LOOP_NEXT_TOOL && toolName !== LOOP_STOP_TOOL) return null;
  if (!hookInput.agent_id) return null;
  return 'Only the orchestrator paces or stops the loop. Say in your report what should happen next.';
}

/** The in-process MCP server with loop_stop, and loop_next when the loop is self-paced. */
export function createLoopServer({ emit, createSdkMcpServer, tool, z, selfPaced }) {
  const tools = [];
  if (selfPaced) {
    tools.push(
      tool(
        'loop_next',
        `Schedule this loop's next run: minutes from now (${NEXT_MIN_MINUTES} to ${NEXT_MAX_MINUTES}) and why. Orchestrator only.`,
        {
          delay_minutes: z.number().int().min(1).max(100000).describe(`Minutes from now; clamped to ${NEXT_MIN_MINUTES}–${NEXT_MAX_MINUTES}`),
          reason: z.string().min(1).max(300).describe('Why then: what the next run should find or do'),
        },
        async ({ delay_minutes, reason }) => {
          const minutes = Math.min(NEXT_MAX_MINUTES, Math.max(NEXT_MIN_MINUTES, Math.round(delay_minutes)));
          emit({ type: 'loop_next', delay_minutes: minutes, reason });
          return { content: [{ type: 'text', text: `Next run scheduled in ${minutes} minutes.` }] };
        },
      ),
    );
  }
  tools.push(
    tool(
      'loop_stop',
      "End this loop: it will not run again until the operator re-enables it. Use when the loop's goal is met. Orchestrator only.",
      { reason: z.string().min(1).max(300).describe('Why the loop should stop') },
      async ({ reason }) => {
        emit({ type: 'loop_stop', reason });
        return { content: [{ type: 'text', text: 'The loop is stopped; this is its last run.' }] };
      },
    ),
  );
  return createSdkMcpServer({ name: LOOP_SERVER, version: '1.0.0', tools });
}
