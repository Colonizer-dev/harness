// The mock's per-call state slice for the chat feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { ChatAttachment, ChatAttachmentNote, ChatImageRef, ChatMessage, ChatMeta, ChatPrefs, ChatStreamEvent } from "../../types";
import { sleep } from "../../mockShared";
import type { MockState } from "../../mockState";

export type ChatMockState = {
    mockChats: Map<string, {
        meta: ChatMeta;
        messages: ChatMessage[];
    }>;
    mockImages: Map<string, {
        url: string;
        ref: ChatImageRef;
    }>;
    mockPrefs: ChatPrefs;
    mockNote: (a: ChatAttachment) => ChatAttachmentNote;
    mockReply: (c: {
        meta: ChatMeta;
        messages: ChatMessage[];
    }, model: string, parent: string | undefined, lane: number | undefined, onEvent: (e: ChatStreamEvent) => void, signal?: AbortSignal) => Promise<void>;
};

export function installChatMockState(ms: MockState): void {
  ms.mockChats = new Map<string, { meta: ChatMeta; messages: ChatMessage[] }>();
  // Chat images: object URLs by a made-up sha, standing in for the mothership's store.
  ms.mockImages = new Map<string, { url: string; ref: ChatImageRef }>();
  ms.mockPrefs = { personas: {}, feedback: {} };
  ms.mockNote = (a: ChatAttachment): ChatAttachmentNote => {
    if (a.kind === "image" && "sha" in a) return { kind: "image", label: a.name || "image", ...ms.mockImages.get(a.sha)?.ref };
    return {
      kind: a.kind,
      label: a.kind === "file" ? `${a.repo}/${a.path}` : a.kind === "snippet" ? a.label || "snippet" : a.kind === "colony" ? a.id : a.kind === "image" ? a.name || a.media_type : a.kind === "map" ? `${a.repo} map` : a.kind === "map_component" ? `${a.repo} · ${a.component}` : a.kind === "merged_prs" ? "merged PRs" : "today's colonies",
    };
  };

  ms.mockReply = async function mockReply(c: { meta: ChatMeta; messages: ChatMessage[] }, model: string, parent: string | undefined, lane: number | undefined, onEvent: (e: ChatStreamEvent) => void, signal?: AbortSignal): Promise<void> {
    const started = Date.now();
    const tag = <T extends object>(e: T) => (lane === undefined ? e : { ...e, lane });
    const answer = `*(mock reply from ${model})* You asked: **${c.messages.at(-1)?.content.slice(0, 80) ?? ""}**\n\nSee \`src/main.rs\` for the entry point.\n\n\`\`\`rust\nfn main() {\n    let answer: u32 = 42; // the answer\n    println!("{answer}");\n}\n\`\`\``;
    let text = "";
    let first: number | undefined;
    for (const word of answer.split(/(?<= )/)) {
      if (signal?.aborted) break;
      await sleep(lane === 1 ? 45 : 30);
      first ??= Date.now() - started;
      text += word;
      onEvent(tag({ type: "delta" as const, text: word }));
    }
    const message: ChatMessage = { id: ms.mockId(), role: "assistant", content: text, ts: new Date().toISOString(), model, input_tokens: 120, output_tokens: 40, cost_usd: 0.0004, stopped: Boolean(signal?.aborted), parent_id: parent, first_token_ms: first, latency_ms: Date.now() - started, candidate: lane !== undefined || undefined, lane };
    c.messages.push(message);
    c.meta = { ...c.meta, updated_at: message.ts };
    let chat: ChatMeta | undefined;
    if (c.meta.auto_title && lane === undefined && c.messages.filter((m) => m.role === "assistant").length === 1) {
      c.meta = { ...c.meta, title: `About ${c.messages[0]?.content.slice(0, 24) ?? "this"}`, auto_title: false };
      chat = c.meta;
    }
    onEvent(tag({ type: "done" as const, message, chat }));
  }
}
