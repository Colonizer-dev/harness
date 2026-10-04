// Colony-to-colony coordination, runner side (issue #834). Several colonies of the same repository
// can be in flight at once; the mothership gateway is the meeting point. A colony declares the paths
// it means to change, sees which other active colony holds or has changed them, and can message the
// others. The gateway, not the colony, decides who is active and what they hold.

export const COORDINATION_SERVER = 'colonizer_coord';

export const COORDINATION_PROMPT_APPEND =
  '- Other colonies may be changing this repository at the same time. Before editing, call `claim` with the paths you plan to change; if another colony holds one, wait, agree an order or split with `send`/`inbox`, or keep your edits in that file minimal and additive.';

// A hung gateway must not hold a model turn for minutes.
const TIMEOUT_MS = 20_000;
const MAX_PATHS = 200;
const MAX_REASON = 4000;
const MAX_TO = 200;
const MAX_TEXT = 4000;

const text = (value) => ({ content: [{ type: 'text', text: value }] });
const error = (message) => ({ isError: true, content: [{ type: 'text', text: message }] });

/**
 * Builds the in-process MCP server with the coordination tools. The SDK helpers and zod are injected
 * so tests can run without the SDK transport, and fetch so they run without the network.
 */
export function createCoordinationServer({ url, token, fetchImpl = fetch, createSdkMcpServer, tool, z }) {
  /** Every op is one JSON POST; a reply is returned pretty-printed, an error is a short text, never a throw. */
  async function post(body) {
    let response;
    try {
      response = await fetchImpl(url, {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(TIMEOUT_MS),
      });
    } catch {
      return error('coordinate: the gateway is unreachable right now.');
    }
    if (!response.ok) {
      let detail = '';
      try {
        detail = (await response.text()).trim();
      } catch {
        // No body to quote; the status is enough.
      }
      return error(detail ? `coordinate: the gateway answered ${response.status}: ${detail}` : `coordinate: the gateway answered ${response.status}.`);
    }
    try {
      return text(JSON.stringify(await response.json(), null, 2));
    } catch {
      return error('coordinate: the gateway answered with something that is not JSON.');
    }
  }

  const claim = tool(
    'claim',
    'Declare the repository paths you intend to change. Returns any other active colony in this repo that holds or has changed the same paths, so you can wait, coordinate with `send`, or proceed with minimal, additive edits.',
    {
      paths: z.array(z.string().min(1)).min(1).max(MAX_PATHS).describe('Repository-relative paths you intend to change'),
      reason: z.string().min(1).max(MAX_REASON).describe('Short note on what you are doing to these paths, for the other colonies'),
    },
    async ({ paths, reason }) => post({ op: 'claim', paths, reason }),
  );

  const claims = tool(
    'claims',
    'List which active colonies in this repository hold which paths, so you can see who else is working where.',
    {},
    async () => post({ op: 'claims' }),
  );

  const send = tool(
    'send',
    'Message another active colony in the same repository, addressed by its colony id or issue number (e.g. "831"). Never include secrets.',
    {
      to: z.string().min(1).max(MAX_TO).describe("The other colony's id or issue number"),
      text: z.string().min(1).max(MAX_TEXT).describe('What to tell the other colony; never secrets'),
    },
    async ({ to, text: message }) => post({ op: 'send', to, text: message }),
  );

  const inbox = tool(
    'inbox',
    'Read the messages other colonies in this repository sent you.',
    {},
    async () => post({ op: 'inbox' }),
  );

  return createSdkMcpServer({ name: COORDINATION_SERVER, version: '1.0.0', tools: [claim, claims, send, inbox] });
}
