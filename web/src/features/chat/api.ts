// Chat API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request, streamNdjson, uploadWithProgress } from "../../http";
import type { ChatCompareRequest, ChatImageRef, ChatMessage, ChatMeta, ChatModels, ChatPatch, ChatPrefs, ChatSendRequest, ChatStreamEvent } from "./types";

export interface ChatApi {
  /** Chat (docs/protocol.md): direct conversations with a model, stored on the mothership. */
  chats(): Promise<{ chats: ChatMeta[] }>;
  chatModels(): Promise<ChatModels>;
  createChat(body: { title?: string; model?: string; system?: string; max_tokens?: number; temperature?: number; persona?: string; workspace?: string }): Promise<ChatMeta>;
  chat(id: string): Promise<{ chat: ChatMeta; messages: ChatMessage[] }>;
  patchChat(id: string, body: ChatPatch): Promise<ChatMeta>;
  deleteChat(id: string): Promise<unknown>;
  /** Streams the reply; `onEvent` gets each line; aborting `signal` stops the reply (kept as stopped). */
  sendChat(id: string, body: ChatSendRequest, onEvent: (event: ChatStreamEvent) => void, signal?: AbortSignal): Promise<void>;
  /** One message to two models at once; every streamed line carries its `lane`. */
  compareChat(id: string, body: ChatCompareRequest, onEvent: (event: ChatStreamEvent) => void, signal?: AbortSignal): Promise<void>;
  /** Keeps one compare reply and drops its sibling. */
  pickChat(id: string, messageId: string): Promise<{ messages: ChatMessage[] }>;
  /** A new conversation with the messages up to (`include`) or just before one of this one's. */
  forkChat(id: string, messageId: string, include: boolean): Promise<ChatMeta>;
  retitleChat(id: string): Promise<ChatMeta>;
  /** The URL of the conversation's Markdown export (a download). */
  /** The Markdown export; `zip` packs it with the conversation's images beside it. */
  chatExportUrl(id: string, zip?: boolean): string;
  /** POST /api/chat/attachments: stores one image (checked by its bytes, metadata stripped) and answers its reference. */
  uploadChatImage(file: Blob, onProgress?: (fraction: number) => void, signal?: AbortSignal): Promise<ChatImageRef>;
  /** GET /api/chat/attachments/{sha}: where a stored image is served. */
  chatImageUrl(sha: string): string;
  chatPrefs(): Promise<ChatPrefs>;
  /** Saves a persona preset's system prompt; `null` goes back to the built-in one. */
  saveChatPersona(id: string, system: string | null): Promise<ChatPrefs>;
  /** Keeps a note on a reply; `null` clears it. */
  saveChatFeedback(messageId: string, note: string | null): Promise<ChatPrefs>;
  /** Files the issue with the Source include labels (as Colonize does); `labels_skipped` are any the repository could not be given. */
  chatIssue(id: string, body: { repo: string; title: string; body: string }): Promise<{ url: string; labels?: string[]; labels_skipped?: string[] }>;
}

export const chatHttp: ChatApi = {
  chats: () => request("/api/chat"),
  chatModels: () => request("/api/chat/models"),
  createChat: (body) => post("/api/chat", body),
  chat: (id) => request(`/api/chat/${enc(id)}`),
  patchChat: (id, body) => request(`/api/chat/${enc(id)}`, { method: "PATCH", body: JSON.stringify(body) }),
  deleteChat: (id) => del(`/api/chat/${enc(id)}`),
  sendChat: (id, body, onEvent, signal) => streamNdjson(`/api/chat/${enc(id)}/messages`, body, onEvent, signal),
  compareChat: (id, body, onEvent, signal) => streamNdjson(`/api/chat/${enc(id)}/compare`, body, onEvent, signal),
  pickChat: (id, messageId) => post(`/api/chat/${enc(id)}/pick`, { message_id: messageId }),
  forkChat: (id, messageId, include) => post(`/api/chat/${enc(id)}/fork`, { message_id: messageId, include }),
  retitleChat: (id) => post(`/api/chat/${enc(id)}/title`),
  chatExportUrl: (id, zip) => `/api/chat/${enc(id)}/export${zip ? "?format=zip" : ""}`,
  uploadChatImage: (file, onProgress, signal) => uploadWithProgress("/api/chat/attachments", file, onProgress, signal),
  chatImageUrl: (sha) => `/api/chat/attachments/${enc(sha)}`,
  chatPrefs: () => request("/api/chat/prefs"),
  saveChatPersona: (id, system) => put(`/api/chat/prefs/personas/${enc(id)}`, { system }),
  saveChatFeedback: (messageId, note) => put(`/api/chat/prefs/feedback/${enc(messageId)}`, { note }),
  chatIssue: (id, body) => post(`/api/chat/${enc(id)}/issue`, body),
};
