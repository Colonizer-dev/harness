// ---------------------------------------------------------------------------
// Chat: a direct conversation with a model, no colony (GET/POST /api/chat, docs/protocol.md)
// ---------------------------------------------------------------------------

export interface ChatMeta {
  id: string;
  title: string;
  model: string;
  system?: string;
  max_tokens: number;
  /** The workspace its spend is filed under; absent files it under the `chat` pseudo-org. */
  workspace?: string;
  created_at: string;
  updated_at: string;
  /** Kept at the top of the list; absent from an older mothership. */
  pinned?: boolean;
  /** 0–1; absent leaves it to the provider. */
  temperature?: number;
  /** The persona preset the system prompt came from, a label only. */
  persona?: string;
  /** The title is still the automatic one; the first reply replaces it with a generated one. */
  auto_title?: boolean;
  forked_from?: { chat: string; message: string };
}

/** What an attachment left on the message it came with: never the content, only what it was. An image
 * also carries its stored reference, so it can be shown and sent to the model again. */
export interface ChatAttachmentNote {
  kind: string;
  label: string;
  sha?: string;
  mime?: string;
  width?: number;
  height?: number;
  bytes?: number;
}

/** A stored chat image (POST /api/chat/attachments), content-addressed by its sha256. */
export interface ChatImageRef {
  sha: string;
  mime: string;
  width: number;
  height: number;
  bytes: number;
}

/** Persona preset edits and notes on replies, kept on the mothership (GET /api/chat/prefs). */
export interface ChatPrefs {
  /** Preset id → the system prompt saved to it. */
  personas: Record<string, string>;
  /** Reply message id → the operator's note on it. */
  feedback: Record<string, string>;
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  ts: string;
  model?: string;
  input_tokens: number;
  output_tokens: number;
  cost_usd?: number;
  stopped: boolean;
  error?: string;
  parent_id?: string;
  first_token_ms?: number;
  latency_ms?: number;
  attachments?: ChatAttachmentNote[];
  /** A compare reply not picked yet; the model's history leaves it out. */
  candidate?: boolean;
  lane?: number;
}

export interface ChatProvider {
  id: string;
  name: string;
  models: string[];
  preset?: string;
  wire?: "anthropic" | "openai";
  has_key?: boolean;
  pricing?: { input_per_mtok: number; output_per_mtok: number } | null;
}

export interface ChatModels {
  /** The cheap default (the summaries' model), or null when none is reachable. */
  default: string | null;
  /** Plain Claude models: usable only with an Anthropic API key or an Anthropic provider. */
  claude: { available: boolean; reason: string | null };
  providers: ChatProvider[];
}

/** Something attached to a message (docs/protocol.md, "Chat attachments"). */
export type ChatAttachment =
  | { kind: "colony"; id: string }
  | { kind: "file"; repo: string; path: string; ref?: string }
  | { kind: "map"; repo: string }
  | { kind: "map_component"; repo: string; component: string }
  | { kind: "snippet"; label?: string; text: string }
  /** A stored image (`sha`), or older clients' inline base64 `data`, which the mothership stores first. */
  | { kind: "image"; sha: string; name?: string }
  | { kind: "image"; media_type: string; data: string; name?: string }
  | { kind: "colonies_today"; org?: string }
  | { kind: "merged_prs"; org?: string; days?: number };

/** One line of the streamed reply to POST /api/chat/{id}/messages (or /compare, tagged by `lane`). */
export type ChatStreamEvent = (
  | { type: "delta"; text: string }
  | { type: "done"; message: ChatMessage; chat?: ChatMeta }
  | { type: "error"; message: string; message_record?: ChatMessage }
) & { lane?: number };

export interface ChatSendRequest {
  content?: string;
  regenerate?: boolean;
  /** Answer with this model once; the conversation keeps its own. */
  model?: string;
  context?: { colony?: string; file?: { repo: string; path: string; ref?: string } };
  attachments?: ChatAttachment[];
}

export interface ChatCompareRequest {
  content: string;
  models: [string, string];
  attachments?: ChatAttachment[];
}

export type ChatPatch = Partial<Pick<ChatMeta, "title" | "model" | "system" | "max_tokens" | "workspace" | "pinned" | "persona">> & {
  /** A negative temperature clears it. */
  temperature?: number;
};
