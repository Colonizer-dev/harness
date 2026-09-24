import { describe, expect, it } from "vitest";
import { splitNdjson } from "../api";
import type { ChatMessage, ChatModels } from "../types";
import { blockedReason, conversationAsInstructions } from "./ChatView";

const models = (available: boolean): ChatModels => ({
  default: "zai/glm-5.3-flash",
  claude: { available, reason: available ? null : "needs an API key" },
  providers: [{ id: "zai", name: "Z.AI", models: ["glm-5.3-flash"] }],
});

const msg = (role: "user" | "assistant", content: string, extra: Partial<ChatMessage> = {}): ChatMessage => ({
  id: `${role}-${content}`,
  role,
  content,
  ts: "2026-09-24T00:00:00Z",
  input_tokens: 0,
  output_tokens: 0,
  stopped: false,
  ...extra,
});

describe("chat", () => {
  it("splits streamed ndjson across chunk boundaries and keeps the unfinished tail", () => {
    const first = splitNdjson('{"type":"delta","text":"He"}\n{"type":"del');
    expect(first.events).toEqual([{ type: "delta", text: "He" }]);
    const second = splitNdjson(first.rest + 'ta","text":"llo"}\n');
    expect(second.events).toEqual([{ type: "delta", text: "llo" }]);
    expect(second.rest).toBe("");
    expect(splitNdjson("not json\n").events).toEqual([]);
  });

  it("blocks a plain Claude model without Claude access, never a provider model", () => {
    expect(blockedReason("claude-haiku-4-5", models(false))).toBe("needs an API key");
    expect(blockedReason("claude-haiku-4-5", models(true))).toBeNull();
    expect(blockedReason("zai/glm-5.3-flash", models(false))).toBeNull();
    expect(blockedReason("", models(true))).toBe("Pick a model.");
  });

  it("turns a conversation into colony instructions up to the chosen reply, without errors", () => {
    const messages = [msg("user", "fix the flaky test"), msg("assistant", "it races"), msg("assistant", "", { error: "boom" }), msg("user", "later")];
    const text = conversationAsInstructions(null, messages, "assistant-it races");
    expect(text).toContain("Me:\nfix the flaky test");
    expect(text).toContain("Assistant:\nit races");
    expect(text).not.toContain("later");
  });
});
