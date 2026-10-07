// GitHub writes, runner side (issue #778, PR actions issue #807, docs/protocol.md §6.12). A colony
// launched by a loop whose work is GitHub's has no GitHub token — the whole design — so it reads what
// the mothership fetched into the read-only /colonizer/github and asks the host for every write: the
// token never enters a colony. Only the orchestrator may ask, and the mothership validates, caps per
// colony and scopes each request to the colony's own repository.

export const GITHUB_SERVER = 'colonizer_github';
export const GITHUB_TOOLS = [
  `mcp__${GITHUB_SERVER}__issue_label`,
  `mcp__${GITHUB_SERVER}__issue_comment`,
  `mcp__${GITHUB_SERVER}__issue_close_duplicate`,
  `mcp__${GITHUB_SERVER}__pr_comment`,
  `mcp__${GITHUB_SERVER}__pr_label`,
  `mcp__${GITHUB_SERVER}__pr_merge`,
];

/** A merge request's head commit: a full 40-character SHA-1 (either case; the host lowercases it), so the host can re-check it on the live PR. */
export const HEAD_SHA_PATTERN = /^[0-9a-fA-F]{40}$/;
export const QUEUED_REPLY =
  'Sent to the harness, which makes the call on this repository unless external writes are blocked or this colony has already made its limit. The outcome appears in the colony log, not here.';

export const GITHUB_PROMPT_APPEND = [
  '- This loop works on GitHub and you have no GitHub token. Read the read-only context the mothership fetched for you under /colonizer/github — issues.json, ci-failures.json and merged-prs.json, each with a "since" timestamp — instead of running `gh`, which cannot authenticate here.',
  '- To label an issue, comment on one, or close a duplicate, use the colonizer_github tools issue_label, issue_comment and issue_close_duplicate; to comment on, label or merge a pull request, use pr_comment, pr_label and pr_merge. Name the issue or pull request numbers you read in the context files; the harness makes the call on this repository and nothing else, and it does not let you choose the repository. pr_merge is refused unless the operator has switched merges on for this repository.',
  '- Those tools belong to the orchestrator: a subagent that finds an issue to label or answer or a pull request to act on reports it, and you decide. The host caps how many writes one colony may make and says so in the log when the cap is reached.',
].join('\n');

/** Why a colonizer_github call is refused, or null. Subagents report; the orchestrator asks. */
export function githubDecision(toolName, hookInput = {}) {
  if (!GITHUB_TOOLS.includes(toolName) || !hookInput.agent_id) return null;
  return 'Only the orchestrator writes to GitHub. Put what you found in your report, and the orchestrator will decide what to label, comment on, close or merge.';
}

/**
 * Builds the in-process MCP server with the six write tools.
 * The SDK helpers and zod are injected so tests can run without the SDK transport.
 */
export function createGithubServer({ emit, createSdkMcpServer, tool, z }) {
  const label = tool(
    'issue_label',
    'Add labels to an issue on this repository. The harness makes the call with its own credentials; the repository is always the colony\'s own; you only name the issue and the labels.',
    {
      issue: z.number().int().positive().describe('The issue number, as it appears in /colonizer/github/issues.json'),
      labels: z.array(z.string().min(1)).min(1).max(10).describe('The labels to add'),
    },
    async ({ issue, labels }) => {
      emit({ type: 'github_action', tool: 'issue_label', issue, labels });
      return { content: [{ type: 'text', text: QUEUED_REPLY }] };
    },
  );
  const comment = tool(
    'issue_comment',
    'Comment on an issue on this repository. The harness makes the call with its own credentials; the repository is always the colony\'s own; you only write the comment.',
    {
      issue: z.number().int().positive().describe('The issue number, as it appears in /colonizer/github/issues.json'),
      body: z.string().min(1).max(20000).describe('Markdown: the comment, as if you were writing it on the issue'),
    },
    async ({ issue, body }) => {
      emit({ type: 'github_action', tool: 'issue_comment', issue, body });
      return { content: [{ type: 'text', text: QUEUED_REPLY }] };
    },
  );
  const closeDuplicate = tool(
    'issue_close_duplicate',
    'Close an issue as a duplicate of another on this repository: the harness comments "Duplicate of #N" and closes it as not planned. The harness makes the call with its own credentials; the repository is always the colony\'s own.',
    {
      issue: z.number().int().positive().describe('The duplicate issue to close'),
      duplicate_of: z.number().int().positive().describe('The issue it duplicates. A different issue number.'),
    },
    async ({ issue, duplicate_of }) => {
      emit({ type: 'github_action', tool: 'issue_close_duplicate', issue, duplicate_of });
      return { content: [{ type: 'text', text: QUEUED_REPLY }] };
    },
  );
  const prComment = tool(
    'pr_comment',
    'Comment on a pull request on this repository. The harness makes the call with its own credentials; the repository is always the colony\'s own; you only write the comment.',
    {
      pr: z.number().int().positive().describe('The pull request number, as it appears in /colonizer/github/merged-prs.json'),
      body: z.string().min(1).max(20000).describe('Markdown: the comment, as if you were writing it on the pull request'),
    },
    async ({ pr, body }) => {
      emit({ type: 'github_action', tool: 'pr_comment', pr, body });
      return { content: [{ type: 'text', text: QUEUED_REPLY }] };
    },
  );
  const prLabel = tool(
    'pr_label',
    'Add labels to a pull request on this repository. The harness makes the call with its own credentials; the repository is always the colony\'s own; you only name the pull request and the labels.',
    {
      pr: z.number().int().positive().describe('The pull request number, as it appears in /colonizer/github/merged-prs.json'),
      labels: z.array(z.string().min(1).max(100)).min(1).max(10).describe('The labels to add'),
    },
    async ({ pr, labels }) => {
      emit({ type: 'github_action', tool: 'pr_label', pr, labels });
      return { content: [{ type: 'text', text: QUEUED_REPLY }] };
    },
  );
  const prMerge = tool(
    'pr_merge',
    'Merge a pull request on this repository. The harness makes the call with its own credentials; the repository is always the colony\'s own. It is refused unless the operator has enabled merges for this repository, and it only merges a pull request that is open, not a draft, from this repository, still at head_sha, mergeable, with every check green, targeting the default branch, touching nothing under .github/ and no credential-like file; at most five merges per colony. The outcome appears in the colony log, not here.',
    {
      pr: z.number().int().positive().describe('The pull request number, as it appears in /colonizer/github/merged-prs.json'),
      head_sha: z.string().regex(HEAD_SHA_PATTERN).describe('The full 40-character hex SHA of the head commit you reviewed; the merge is refused if the pull request has moved on'),
      reason: z.string().min(1).max(500).describe('One short line: why this pull request is safe to merge now'),
    },
    async ({ pr, head_sha, reason }) => {
      emit({ type: 'github_action', tool: 'pr_merge', pr, head_sha, reason });
      return { content: [{ type: 'text', text: QUEUED_REPLY }] };
    },
  );
  return createSdkMcpServer({
    name: GITHUB_SERVER,
    version: '1.0.0',
    tools: [label, comment, closeDuplicate, prComment, prLabel, prMerge],
  });
}
