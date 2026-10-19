// Living Brain (crates/colonizer/src/living_brain): the team brain's MCP server, for a colony whose
// repository's rule asks for it. The mothership sets COLONIZER_LIVINGBRAIN_MCP_URL and hands the
// token to msb as a secret for the MCP host alone, so COLONIZER_LIVINGBRAIN_TOKEN here holds msb's
// placeholder, never the value: the header carries the placeholder and msb swaps the token in on TLS
// to that host. Sent anywhere else, the placeholder is blocked.

export const LIVINGBRAIN_SERVER = 'livingbrain';
export const LIVINGBRAIN_URL_ENV = 'COLONIZER_LIVINGBRAIN_MCP_URL';
export const LIVINGBRAIN_TOKEN_ENV = 'COLONIZER_LIVINGBRAIN_TOKEN';

export const LIVINGBRAIN_PROMPT_APPEND = [
  '',
  '<living-brain>',
  "Your team's brain (Living Brain: its wiki and memory) is available as the mcp__livingbrain__* tools.",
  '- Before you start, call brain_context_for with this repository and your task: earlier decisions, conventions and gotchas may already be written down. Cite what you use.',
  '- brain_search and brain_page find and read pages; brain_note saves a short note to your own private memory.',
  '- Note only durable, reusable facts (a decision and its reason, a trap and its fix) — never secrets, tokens or customer data.',
  '</living-brain>',
].join('\n');

/** The remote MCP server entry for the Claude Agent SDK, or null when the mothership did not grant one. */
export function livingBrainServer(env = {}) {
  const url = String(env[LIVINGBRAIN_URL_ENV] ?? '').trim();
  const token = String(env[LIVINGBRAIN_TOKEN_ENV] ?? '').trim();
  if (!url.startsWith('https://') || !token) return null;
  return { type: 'http', url, headers: { Authorization: `Bearer ${token}` } };
}
