// Hand-off logic (issue #738): the pure parts of moving a conversation between a developer's
// machine and a colony. `parseHandoffFile` is the only way an untrusted upload becomes a transcript
// (the server enforces the same limits again); `continueLocallyCommands` builds the two commands
// the "Continue locally" panel shows. Kept out of the components so the fixtures here can test it.
import type { SimpleContentBlock, SimpleTranscript } from "./types";

/** The cap on an uploaded transcript — the server's 2 MiB limit, applied here too so a huge file never leaves the browser. */
export const MAX_HANDOFF_BYTES = 2 * 1024 * 1024;
/** The name the export downloads as and the commands read. */
export const HANDOFF_FILE = "colony.json";

/** UTF-8 byte length, which is what the server's 2 MiB limit counts. */
export function transcriptBytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** `owner/name` — what every launch form needs, and what the hand-off route refuses without (400). */
export function repoValid(repo: string): boolean {
  return /^[\w.-]+\/[\w.-]+$/.test(repo.trim());
}

/**
 * The value of a Simple JSON document when it is one, else null. The contract is only this: a JSON
 * object (not an array) whose `messages` is an array. Deeper shape is the agent's business.
 */
export function asSimpleTranscript(value: unknown): SimpleTranscript | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  return Array.isArray((value as { messages?: unknown }).messages) ? (value as SimpleTranscript) : null;
}

/** A parsed upload, or the reason it was refused. */
export type ParsedHandoff = { transcript: SimpleTranscript; branch: string | null; title: string | null } | { error: string };

/**
 * The uploaded file's text as a transcript, plus the two fields the launch form prefills from it
 * (the top-level `git_branch` and `title`). Rejects anything over the cap, anything that is not
 * JSON, and anything without a `messages` array, with a message worth showing the operator.
 */
export function parseHandoffFile(text: string): ParsedHandoff {
  if (transcriptBytes(text) > MAX_HANDOFF_BYTES) return { error: `That file is larger than ${MAX_HANDOFF_BYTES / (1024 * 1024)} MiB — the limit for an uploaded session.` };
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return { error: "That file is not valid JSON." };
  }
  const transcript = asSimpleTranscript(value);
  if (!transcript) return { error: "That is not a txcript Simple JSON export: it has no messages array." };
  return { transcript, branch: prefill(transcript.git_branch), title: prefill(transcript.title) };
}

/** A trimmed non-empty string, else null — how a blank `git_branch`/`title` reads as "no prefill". */
function prefill(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null;
}

/** The `txcript continue --with` value for a colony's agent module id. */
export function txcriptAgent(agent: string | undefined): string {
  const a = (agent ?? "").toLowerCase();
  if (a.includes("codex")) return "codex";
  if (a.includes("opencode")) return "opencode";
  return "claude_code";
}

// Codex's tool names are lower-case and shell-shaped; Claude Code's are capitalised verbs. Matching
// them is how the --with value stays honest when the colony's own agent did not write the transcript.
const CODEX_TOOLS = new Set(["shell", "exec_command", "local_shell", "apply_patch"]);
const CLAUDE_TOOLS = new Set(["bash", "read", "edit", "write", "glob", "grep", "task", "todowrite", "webfetch", "notebookedit"]);

/** Which agent wrote a transcript, read from its tool names; null when nothing is recognisable. */
export function inferAgent(transcript: SimpleTranscript): "claude_code" | "codex" | null {
  let codex = 0;
  let claude = 0;
  for (const message of transcript.messages) {
    if (typeof message.content === "string") continue;
    for (const block of message.content as SimpleContentBlock[]) {
      const name = block?.type === "tool_use" ? String(block.name ?? "").toLowerCase() : "";
      if (CODEX_TOOLS.has(name)) codex += 1;
      else if (CLAUDE_TOOLS.has(name)) claude += 1;
    }
  }
  if (claude > 0 && codex <= claude) return "claude_code";
  return codex > 0 ? "codex" : null;
}

/** The two commands the panel shows: bring the colony branch down and switch to it, then replay the export. */
export function continueLocallyCommands(opts: { branch: string; withAgent: string; file?: string }): string[] {
  const file = opts.file ?? `./${HANDOFF_FILE}`;
  return [`git fetch origin ${opts.branch} && git switch ${opts.branch}`, `txcript continue ${file} --with ${opts.withAgent}`];
}

/** The commands for a colony, preferring the agent an exported transcript reveals over the colony's own setting. */
export function locallyCommandsFor(session: { branch: string; agent?: string }, transcript?: SimpleTranscript | null): string[] {
  const withAgent = (transcript ? inferAgent(transcript) : null) ?? txcriptAgent(session.agent);
  return continueLocallyCommands({ branch: session.branch, withAgent });
}
