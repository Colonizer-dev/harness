import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { splitNdjson, type Api } from "../api";
import { ApiContext } from "../context";
import type { ChatMessage, ChatModels, Repo } from "../types";
import { MessageRow } from "./chat/Message";
import { ChatView, blockedReason, conversationAsInstructions, suggestionsFor } from "./ChatView";
import { session } from "./testFixtures";

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

  it("leaves unpicked compare replies out of colony instructions", () => {
    const messages = [msg("user", "q"), msg("assistant", "candidate", { candidate: true }), msg("assistant", "kept")];
    expect(conversationAsInstructions(null, messages)).not.toContain("candidate");
  });

  it("suggests from the workspace: its latest repo's map, today's colonies, a failed colony, release notes, file and issue pickers", () => {
    const repos: Repo[] = [{ full_name: "acme/web", description: null, private: false, fork: false, archived: false, open_issues_count: 0, pushed_at: "2026-09-01" }];
    const failed = session({ id: "f1", repo: "acme/api", status: "failed", summary: "Fix the flaky login test", updated_at: "2026-09-24T10:00:00Z" });
    const s = suggestionsFor("acme", repos, [failed, session({ id: "o1", repo: "octo/site", updated_at: "2026-09-24T11:00:00Z" })]);
    expect(s.map((x) => x.id)).toEqual(["architecture", "today", "failed", "release", "review", "plan"]);
    expect(s[0].attach).toEqual({ kind: "map", repo: "acme/api" });
    expect(s[2].attach).toEqual({ kind: "colony", id: "f1" });
    expect(s[3].attach).toEqual({ kind: "merged_prs", org: "acme", days: 7 });
    expect(s[4].step).toBe("file-repo");
    // Nothing failed and no repositories: only what still makes sense.
    expect(suggestionsFor(null, [], []).map((x) => x.id)).toEqual(["today", "release", "review", "plan"]);
  });

  it("renders the empty state with its hero, composer and suggestions — and no native selects", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={{} as Api}>
        <ChatView org="acme" repos={[]} sessions={[session({ repo: "acme/web" })]} autopilotDefault onCreated={() => {}} />
      </ApiContext.Provider>,
    );
    expect(html).toContain("What do you want to know?");
    expect(html).toContain('aria-label="message"');
    expect(html).toContain('aria-label="attach context"');
    expect(html).toContain("Explain web&#x27;s architecture");
    expect(html).toContain("Search conversations");
    expect(html).not.toContain("<select");
  });

  it("shows a reply's model, tokens, cost and timing, and its attachments", () => {
    const m = msg("assistant", "See `src/main.rs`.", {
      model: "zai/glm-5.3-flash",
      input_tokens: 1200,
      output_tokens: 300,
      cost_usd: 0.0012,
      first_token_ms: 420,
      latency_ms: 2300,
      attachments: [{ kind: "file", label: "acme/web/src/main.rs" }],
    });
    const html = renderToStaticMarkup(
      <MessageRow m={m} models={models(true)} claudeIds={[]} isLastReply busy={false} hit={null} onAction={() => {}} onOpenFile={() => {}} />,
    );
    expect(html).toContain("glm-5.3-flash");
    expect(html).toContain("first token 420 ms");
    expect(html).toContain("2.3 s");
    expect(html).toContain("acme/web/src/main.rs");
    expect(html).toContain('title="Open src/main.rs in Code"');
    expect(html).toContain('aria-label="Regenerate with another model"');
    expect(html).toContain('aria-label="Create a GitHub issue"');
  });

  it("shows stored images as thumbnails that open full size, and the mothership's note on a reply", () => {
    const sha = "a".repeat(64);
    const user = msg("user", "what is this?", { attachments: [{ kind: "image", label: "cat.png", sha, mime: "image/png", width: 640, height: 480, bytes: 1000 }, { kind: "file", label: "acme/web/x.rs" }] });
    const html = renderToStaticMarkup(
      <MessageRow m={user} models={null} claudeIds={[]} isLastReply={false} busy={false} hit={null} onAction={() => {}} imageUrl={(s) => `/api/chat/attachments/${s}`} />,
    );
    expect(html).toContain(`src="/api/chat/attachments/${sha}"`);
    expect(html).toContain('aria-label="open image cat.png"');
    expect(html).toContain("acme/web/x.rs");
    const reply = renderToStaticMarkup(
      <MessageRow m={msg("assistant", "a dog")} models={null} claudeIds={[]} isLastReply busy={false} hit={null} onAction={() => {}} note="it is a cat" />,
    );
    expect(reply).toContain("Your note: it is a cat");
  });

  it("tells a colony which images the conversation had", () => {
    const text = conversationAsInstructions(null, [msg("user", "fix this layout", { attachments: [{ kind: "image", label: "shot.png", sha: "b".repeat(64) }] })]);
    expect(text).toContain("[image shared in the chat, not attached: shot.png]\nfix this layout");
  });

  it("offers a compare candidate to be picked instead of the usual actions", () => {
    const html = renderToStaticMarkup(
      <MessageRow m={msg("assistant", "A", { candidate: true, lane: 0 })} models={null} claudeIds={[]} isLastReply={false} busy={false} hit={null} onAction={() => {}} />,
    );
    expect(html).toContain("Use this reply");
    expect(html).not.toContain("Branch from here");
  });
});
