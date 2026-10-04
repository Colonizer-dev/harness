// The `chat` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { sleep } from "../../mockShared";
import type { ChatImageRef, ChatMessage, ChatMeta } from "../../types";
import type { MockState } from "../../mockState";
import type { ChatApi } from "./api";

export function chatMock(ms: MockState): ChatApi {
  return {
    chats: () => ms.later(() => ({ chats: [...ms.mockChats.values()].map((c) => c.meta).sort((a, b) => b.updated_at.localeCompare(a.updated_at)) })),
    chatModels: () =>
      ms.later(() => ({
        default: "zai/glm-5.3-flash",
        claude: { available: false, reason: "a Claude model needs an Anthropic API key (sk-ant-api…) or an Anthropic model provider; the Claude subscription login is only used by colonies" },
        providers: [
          { id: "zai", name: "Z.AI", models: ["glm-5.3-flash", "glm-5.3"], preset: "zai", wire: "anthropic" as const, has_key: true, pricing: { input_per_mtok: 0.6, output_per_mtok: 2.2 } },
          { id: "deepseek", name: "DeepSeek", models: ["deepseek-chat", "deepseek-reasoner"], preset: "deepseek", wire: "openai" as const, has_key: false, pricing: null },
        ],
      })),
    createChat: (body) =>
      ms.later(() => {
        const at = new Date().toISOString();
        const meta = { id: Math.random().toString(16).slice(2, 10), title: body.title ?? "", model: body.model || "zai/glm-5.3-flash", system: body.system, max_tokens: body.max_tokens ?? 4096, workspace: body.workspace, persona: body.persona || undefined, created_at: at, updated_at: at };
        ms.mockChats.set(meta.id, { meta, messages: [] });
        return meta;
      }),
    chat: (id) =>
      ms.later(() => {
        const c = ms.mockChats.get(id);
        if (!c) throw new Error("no such conversation");
        return { chat: c.meta, messages: [...c.messages] };
      }),
    patchChat: (id, body) =>
      ms.later(() => {
        const c = ms.mockChats.get(id);
        if (!c) throw new Error("no such conversation");
        c.meta = { ...c.meta, ...body, updated_at: new Date().toISOString() };
        return c.meta;
      }),
    deleteChat: (id) => ms.later(() => ms.mockChats.delete(id)),
    sendChat: async (id, body, onEvent, signal) => {
      const c = ms.mockChats.get(id);
      if (!c) throw new Error("no such conversation");
      let parent: string | undefined;
      if (body.regenerate) {
        while (c.messages.at(-1)?.role === "assistant") c.messages.pop();
        parent = c.messages.at(-1)?.id;
      } else if (body.content) {
        const user: ChatMessage = { id: ms.mockId(), role: "user", content: body.content, ts: new Date().toISOString(), input_tokens: 0, output_tokens: 0, stopped: false, attachments: (body.attachments ?? []).map(ms.mockNote) };
        c.messages.push(user);
        parent = user.id;
      }
      if (!c.meta.title && body.content) c.meta = { ...c.meta, title: body.content.slice(0, 60), auto_title: true };
      await ms.mockReply(c, body.model ?? c.meta.model, parent, undefined, onEvent, signal);
    },
    compareChat: async (id, body, onEvent, signal) => {
      const c = ms.mockChats.get(id);
      if (!c) throw new Error("no such conversation");
      const user: ChatMessage = { id: ms.mockId(), role: "user", content: body.content, ts: new Date().toISOString(), input_tokens: 0, output_tokens: 0, stopped: false, attachments: (body.attachments ?? []).map(ms.mockNote) };
      c.messages.push(user);
      if (!c.meta.title) c.meta = { ...c.meta, title: body.content.slice(0, 60) };
      await Promise.all(body.models.map((m, lane) => ms.mockReply(c, m, user.id, lane, onEvent, signal)));
    },
    pickChat: (id, messageId) =>
      ms.later(() => {
        const c = ms.mockChats.get(id);
        if (!c) throw new Error("no such conversation");
        const pick = c.messages.find((m) => m.id === messageId && m.candidate);
        if (!pick) throw new Error("no such compare reply");
        c.messages = c.messages
          .filter((m) => !(m.candidate && m.parent_id === pick.parent_id && m.id !== messageId))
          .map((m) => (m.id === messageId ? { ...m, candidate: false, lane: undefined } : m));
        return { messages: [...c.messages] };
      }),
    forkChat: (id, messageId, include) =>
      ms.later(() => {
        const c = ms.mockChats.get(id);
        if (!c) throw new Error("no such conversation");
        const at = c.messages.findIndex((m) => m.id === messageId);
        if (at < 0) throw new Error("no such message");
        const now = new Date().toISOString();
        const meta: ChatMeta = { ...c.meta, id: ms.mockId(), title: `${c.meta.title || "conversation"} (branch)`, pinned: false, auto_title: false, created_at: now, updated_at: now, forked_from: { chat: id, message: messageId } };
        ms.mockChats.set(meta.id, { meta, messages: c.messages.slice(0, include ? at + 1 : at).filter((m) => !m.candidate) });
        return meta;
      }),
    retitleChat: (id) =>
      ms.later(() => {
        const c = ms.mockChats.get(id);
        if (!c) throw new Error("no such conversation");
        c.meta = { ...c.meta, title: `Mock title for ${c.messages[0]?.content.slice(0, 20) ?? "chat"}`, auto_title: false };
        return c.meta;
      }),
    chatExportUrl: (id) => `data:text/markdown,${encodeURIComponent(`# ${ms.mockChats.get(id)?.meta.title ?? "Conversation"}\n`)}`,
    uploadChatImage: async (file, onProgress) => {
      for (const f of [0.25, 0.6, 1]) {
        await sleep(120);
        onProgress?.(f);
      }
      const sha = [...crypto.getRandomValues(new Uint8Array(32))].map((b) => b.toString(16).padStart(2, "0")).join("");
      const ref: ChatImageRef = { sha, mime: file.type || "image/png", width: 0, height: 0, bytes: file.size };
      ms.mockImages.set(sha, { url: URL.createObjectURL(file), ref });
      return ref;
    },
    chatImageUrl: (sha) => ms.mockImages.get(sha)?.url ?? "",
    chatPrefs: () => ms.later(() => structuredClone(ms.mockPrefs)),
    saveChatPersona: (id, system) =>
      ms.later(() => {
        if (system === null) delete ms.mockPrefs.personas[id];
        else ms.mockPrefs.personas[id] = system;
        return structuredClone(ms.mockPrefs);
      }),
    saveChatFeedback: (messageId, note) =>
      ms.later(() => {
        if (note === null) delete ms.mockPrefs.feedback[messageId];
        else ms.mockPrefs.feedback[messageId] = note;
        return structuredClone(ms.mockPrefs);
      }),
    chatIssue: async (_id, body) => ({ url: `https://github.com/${body.repo}/issues/999`, labels: ms.sourceLabels(), labels_skipped: [] })
  };
}
