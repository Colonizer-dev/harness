// The hand-off feature's types (issue #738): the txcript "Simple JSON" document, and the body of
// POST /api/handoff. See docs/cockpit.md for both flows.

/** One content block of a Simple JSON message, as Claude Code and Codex export them. */
export type SimpleContentBlock =
  | { type: "text"; text: string }
  | { type: "tool_use"; id: string; name: string; input: Record<string, unknown> }
  | { type: "tool_result"; tool_use_id: string; content: unknown }
  | { type: "thinking"; thinking: string };

export interface SimpleMessage {
  role: "user" | "assistant";
  content: string | SimpleContentBlock[];
}

/**
 * A txcript Simple JSON transcript: the smallest shape both a Claude Code and a Codex export share.
 * `git_branch` and `title` prefill the launch form; the rest is handed to the mothership as-is.
 */
export interface SimpleTranscript {
  id?: string;
  timestamp?: string;
  cwd?: string;
  git_branch?: string;
  title?: string;
  model?: string;
  messages: SimpleMessage[];
}

/** POST /api/handoff: continue an uploaded local session as a colony. Answers the same JSON as `POST /api/sessions`. */
export interface HandoffRequest {
  repo: string;
  /** Branch to start from; omit for the repository default. */
  branch?: string;
  title?: string;
  instructions?: string;
  /** The parsed Simple JSON document, verbatim. */
  transcript: SimpleTranscript;
}
