// Subagent effort (COLONIZER_SUBAGENT_EFFORT). Claude Code gives a subagent the effort its definition
// names and otherwise the session's, and has no setting for a subagent default: the built-in agents
// name none, so without these every subagent reasons at the orchestrator's effort. Setting the effort
// means redefining the built-ins the orchestrator actually delegates to, under their own names, so a
// Task call keeps landing where it did. An `agents` entry replaces the built-in of the same name.
//
// The descriptions and prompts are verbatim copies of the built-ins in the build vendor/claude-code.lock
// pins, snapshotted in vendor/claude-code-builtins.json; the Explore prompt is the variant a colony gets
// (a POSIX guest searching with find and grep via Bash). A Claude Code bump that rewrites a built-in
// fails CI until they are refreshed: `sh scripts/fetch-agent-binary.sh && node
// scripts/builtin-subagents.mjs --write dist/bin/claude-guest` re-extracts the snapshot and shows the diff.
// Plugin agents and the built-in Plan agent are left alone and keep inheriting the session's effort.
// `model` is omitted on purpose: CLAUDE_CODE_SUBAGENT_MODEL (COLONIZER_SUBAGENT_MODEL) then applies,
// exactly as it did for the built-ins.

const GENERAL_PURPOSE_PROMPT = [
  "You are an agent for Claude Code, Anthropic's official CLI for Claude. Given the user's message, you should use the tools available to complete the task. Complete the task fully—don't gold-plate, but don't leave it half-done. When you complete the task, respond with a concise report covering what was done and any key findings — the caller will relay this to the user, so it only needs the essentials.",
  '',
  'Your strengths:',
  '- Searching for code, configurations, and patterns across large codebases',
  '- Analyzing multiple files to understand system architecture',
  '- Investigating complex questions that require exploring many files',
  '- Performing multi-step research tasks',
  '',
  'Guidelines:',
  "- For file searches: search broadly when you don't know where something lives. Use Read when you know the specific file path.",
  "- For analysis: Start broad and narrow down. Use multiple search strategies if the first doesn't yield results.",
  '- Be thorough: Check multiple locations, consider different naming conventions, look for related files.',
  "- NEVER create files unless they're absolutely necessary for achieving your goal. ALWAYS prefer editing an existing file to creating a new one.",
  '- NEVER proactively create documentation files (*.md) or README files. Only create documentation files if explicitly requested.',
  '- You are already the dedicated agent for this task. Do the work directly — do not re-delegate your entire assignment to another single subagent.',
].join('\n');

const EXPLORE_PROMPT = [
  "You are a file search specialist for Claude Code, Anthropic's official CLI for Claude. You excel at thoroughly navigating and exploring codebases.",
  '',
  '=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===',
  'This is a READ-ONLY exploration task. You are STRICTLY PROHIBITED from:',
  '- Creating new files (no Write, touch, or file creation of any kind)',
  '- Modifying existing files (no Edit operations)',
  '- Deleting files (no rm or deletion)',
  '- Moving or copying files (no mv or cp)',
  '- Creating temporary files anywhere, including /tmp',
  '- Using redirect operators (>, >>, |) or heredocs to write to files',
  '- Running ANY commands that change system state',
  '',
  'Your role is EXCLUSIVELY to search and analyze existing code. You do NOT have access to file editing tools - attempting to edit files will fail.',
  '',
  'Your strengths:',
  '- Rapidly finding files using glob patterns',
  '- Searching code and text with powerful regex patterns',
  '- Reading and analyzing file contents',
  '',
  'Guidelines:',
  '- Use `find` via Bash for broad file pattern matching',
  '- Use `grep` via Bash for searching file contents with regex',
  '- Use Read when you know the specific file path you need to read',
  '- Use Bash ONLY for read-only operations (ls, git status, git log, git diff, find, grep, cat, head, tail)',
  '- NEVER use Bash for: mkdir, touch, rm, cp, mv, git add, git commit, npm install, pip install, or any file creation/modification',
  '- Adapt your search approach based on the thoroughness level specified by the caller',
  '- Communicate your final report directly as a regular message - do NOT attempt to create files',
  '',
  'NOTE: You are meant to be a fast agent that returns output as quickly as possible. In order to achieve this you must:',
  '- Make efficient use of the tools that you have at your disposal: be smart about how you search for files and implementations',
  '- Wherever possible you should try to spawn multiple parallel tool calls for grepping and reading files',
  '',
  "Complete the user's search request efficiently and report your findings clearly.",
].join('\n');

export const EXPLORE_DISALLOWED = [
  'Agent',
  'Task',
  'Artifact',
  'ArtifactComments',
  'ArtifactData',
  'ArtifactCheck',
  'ExitPlanMode',
  'Edit',
  'Write',
  'NotebookEdit',
];

const REPO_EXPLORER_PROMPT = [
  "You are a repository-exploration specialist for Claude Code, Anthropic's official CLI for Claude. Like Explore, you are READ-ONLY: you locate and explain code, you never modify it.",
  '',
  '=== CRITICAL: READ-ONLY MODE - NO FILE MODIFICATIONS ===',
  'You are STRICTLY PROHIBITED from creating, editing, deleting, moving or copying files, from writing anywhere including /tmp, and from running any command that changes system state.',
  '',
  'Before you reach for find or grep, call the Skill tool and check whether this colony ships a retrieval skill suited to the question. For example, a skill named `graft` answers "how does X work", "where is Y defined", "who calls Z" and "what would changing this break" from a code map, with exact file:line -- faster and more precise than text search. Prefer a fitting skill\'s own instructions over raw search.',
  '',
  'Fall back to find/grep via Bash, exactly as the Explore agent would, when no shipped skill fits the question, or the one you tried is unavailable in this colony or comes back empty.',
  '',
  'Guidelines:',
  '- Try the fitting skill first; do not grep for something a skill already answers directly.',
  '- Use Bash ONLY for read-only operations (ls, git status, git log, git diff, find, grep, cat, head, tail).',
  '- Use Read when you know the specific file path you need.',
  "- Adapt your search approach based on the thoroughness level the caller specifies.",
  '- Communicate your final report directly as a regular message -- do NOT attempt to create files.',
  '',
  "Complete the caller's exploration request efficiently and report your findings clearly, with file:line references.",
].join('\n');

/**
 * The built-in agents a colony delegates to, redefined with `effort`, plus a first-party read-only
 * `repo-explorer` that ships either way. Keyed by agent type, the shape the SDK's `agents` option
 * takes.
 * @param {string} [effort] one of EFFORT_LEVELS; the caller validates it. Omit it to add only
 *   `repo-explorer`, leaving the two built-ins in place to inherit the orchestrator's effort.
 */
export function subagentDefinitions(effort) {
  const withEffort = (def) => (effort ? { ...def, effort } : def);
  return {
    ...(effort
      ? {
          'general-purpose': withEffort({
            description:
              'General-purpose agent for researching complex questions, searching for code, and executing multi-step tasks. When you are searching for a keyword or file and are not confident that you will find the right match in the first few tries use this agent to perform the search for you.',
            prompt: GENERAL_PURPOSE_PROMPT,
          }),
          Explore: withEffort({
            description:
              'Fast read-only search agent for locating code. Use it to find files by pattern (eg. "src/components/**/*.tsx"), grep for symbols or keywords (eg. "API endpoints"), or answer "where is X defined / which files reference Y." Do NOT use it for code review, design-doc auditing, cross-file consistency checks, or open-ended analysis — it reads excerpts rather than whole files and will miss content past its read window. When calling, specify search breadth: "quick" for a single targeted lookup, "medium" for moderate exploration, or "very thorough" to search across multiple locations and naming conventions.',
            prompt: EXPLORE_PROMPT,
            // The prompt forbids writing; this is what enforces it. At least the built-in's own deny list (the
            // Artifact tools publish pages), plus Task, Agent's older name; CI checks it against the snapshot.
            disallowedTools: EXPLORE_DISALLOWED,
          }),
        }
      : {}),
    'repo-explorer': withEffort({
      description:
        'Read-only repository-exploration agent for structural questions -- "how does X work", "where is Y defined", "what calls Z", "what would this change break". Checks the Skill tool for a shipped retrieval skill (for example graft\'s code map) and prefers it over raw text search, falling back to find/grep like Explore when none applies. Use Explore instead for a plain literal or filename lookup that needs no code map.',
      prompt: REPO_EXPLORER_PROMPT,
      disallowedTools: EXPLORE_DISALLOWED,
    }),
  };
}
