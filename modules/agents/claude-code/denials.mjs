/**
 * The denial translation layer (issue #304): when a tool result shows that the colony refused
 * something — a blocked host, a read-only path, a disabled tool — the runner attaches one short,
 * actionable hint to the emitted `tool_result` event, and may repeat the hint to the agent itself.
 *
 * This is GUIDANCE, explicitly not a boundary: hints never change a return code or `is_error`,
 * never grant anything, and removing this layer changes nothing about what the agent can do — it
 * only renames refusals that already happened. Matching is deliberately conservative: only clear
 * sandbox/colony refusal signatures classify, so ordinary tool failures stay unannotated. (A
 * masked-path empty read is not detectable from text and is deliberately not attempted here.)
 */

/** The closed vocabulary of denial classes (docs/agent-events.schema.json `tool_result.denial`). */
export const DENIAL_CLASSES = ['egress', 'read_only', 'tool_disabled'];

/**
 * The guidance each class carries. The same text travels on the event and, once per class per
 * session, back to the agent.
 */
export const DENIAL_HINTS = {
  egress:
    "Network access to that host is refused in this colony (egress is limited to the agent's declared hosts and the task's providers). Don't retry; use what is installed or vendored, and mention the missing access in your report.",
  read_only:
    'That path is read-only in this colony; edit files in the working tree only, and leave commits and installs to the harness.',
  tool_disabled:
    'That tool is disabled for this session; delegate the work with the Task tool or ask via a choice card instead of retrying.',
};

/**
 * Refusal signatures per class, matched case-insensitively against the tool result text. The
 * `tool_disabled` strings are the ones this stack actually produces: Claude Code renders a denied
 * PreToolUse hook as "Permission for this action has been denied. Reason: …" and a denied
 * permission rule as "Permission to use <tool> … has been denied.", and the reasons are this
 * harness's own — delegationDecision in runner.mjs and the loop/findings/memory decisions.
 */
const DENIAL_RULES = [
  [
    'egress',
    [
      /\beconnrefused\b/i,
      /\bconnection refused\b/i,
      /\benotfound\b/i,
      /\bcould not resolve host\b/i,
      /\beai_again\b/i,
      /\bgetaddrinfo\b/i,
      /\benetunreach\b/i,
      /\bnetwork is unreachable\b/i,
      /\bconnect tunnel failed\b/i,
      /\bfrom proxy after connect\b/i,
    ],
  ],
  [
    'read_only',
    [
      /\berofs\b/i,
      /\bread-only file system\b/i,
      /unable to create '[^']*[/\\]\.git[/\\][^']*lock'/i,
      /\binsufficient permission for adding an object\b/i,
    ],
  ],
  [
    'tool_disabled',
    [
      /\bpermission (?:to use [^\n]*|for this action) has been denied\b/i,
      /\btool [^\n]{0,80} is not allowed\b/i,
      /belongs to your subagents in this colony/i,
      /\bonly the orchestrator\b/i,
    ],
  ],
];

/**
 * Classifies a tool result text as a colony refusal, or returns null for anything else. First
 * matching class wins; every hit carries its fixed hint.
 * @param {string} [text]
 * @returns {{ class: string, hint: string } | null}
 */
export function classifyDenial(text) {
  if (typeof text !== 'string' || !text) return null;
  for (const [cls, patterns] of DENIAL_RULES) {
    if (patterns.some((p) => p.test(text))) return { class: cls, hint: DENIAL_HINTS[cls] };
  }
  return null;
}

/**
 * Attaches the denial hint to a built `tool_result` event. Only errored results are considered,
 * and the event's other fields are never touched: with the layer stripped (`classify` returning
 * null) the event is exactly as it was built.
 * @param {object} event            the tool_result event as onUser builds it
 * @param {string} text             the same output text the event carries
 * @param {(text: string) => { class: string, hint: string } | null} [classify]
 */
export function annotateDenial(event, text, classify = classifyDenial) {
  if (!event || !event.is_error) return event;
  const denial = classify(text);
  return denial ? { ...event, denial } : event;
}

/**
 * The agent-facing wording for one or more denial classes, or null for none. This is what the
 * runner delivers through the PostToolUseFailure hook's additionalContext (at most once per class
 * per session), not a protocol event.
 * @param {Iterable<string>} classes
 * @returns {string | null}
 */
export function denialGuidance(classes) {
  const hints = [...new Set(classes)].map((cls) => DENIAL_HINTS[cls]).filter(Boolean);
  if (!hints.length) return null;
  return `Note from the harness about the refusal just above (guidance only; this grants nothing): ${hints.join(' ')}`;
}
