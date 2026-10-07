// The mock's per-call state slice for the chat feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { ChatApproval, ChatAttachment, ChatAttachmentNote, ChatImageRef, ChatMessage, ChatMeta, ChatPrefs, ChatStreamEvent, ChatToolNote } from "../../types";
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
    /** Held writes (#1217), by id: nothing "runs" until the mock's decide settles one. */
    mockApprovals: Map<string, ChatApproval>;
    mockPropose: (tool: string, args: Record<string, unknown>, chat: string, message: string) => ChatApproval;
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
  ms.mockApprovals = new Map<string, ChatApproval>();
  ms.mockPropose = (tool, args, chat, message) => {
    const id = ms.mockId();
    const colony = typeof args.id === "string" ? [...ms.sessions.values()].find((m) => m.session.id === args.id)?.session : undefined;
    const label = colony ? ` “${colony.issue_title || colony.summary || colony.id}” on ${colony.repo}` : "";
    const base = { colonies: colony ? 1 : 0, orgs: colony ? 1 : 0, repos: colony ? 1 : 0 };
    let approval: ChatApproval;
    if (tool === "switch_models") {
      const roles = (args.roles ?? {}) as Record<string, string>;
      const role = Object.keys(roles)[0] ?? "subagent_model";
      const to = roles[role] ?? "byteplus/glm-5.1";
      const running = args.apply === "running";
      approval = {
        id, chat, message, tool, args, status: "pending", created_at: new Date().toISOString(),
        preview: {
          summary: `Switch the ${role.replace(/_/g, " ")} from minimax/MiniMax-M3.1-Flash-Preview to ${to} for all orgs${running ? ", and restart 7 running colonies" : ""}.`,
          diff: [
            { scope: "install", target: "agent", key: role, was: "minimax/MiniMax-M3.1-Flash-Preview", now: to },
            { scope: "org", target: "acme", key: role, was: "minimax/MiniMax-M3.1-Flash-Preview", now: to },
            { scope: "org", target: "octocat", key: role, was: null, now: to },
          ],
          dry_run: true,
          blast: { colonies: running ? 7 : 0, orgs: 3, repos: running ? 5 : 0, note: running ? "7 running colonies restart on the new models." : "Colonies started from now on; running ones are untouched." },
        },
      };
    } else {
      const summaries: Record<string, string> = {
        stop_colony: `Stop colony ${args.id}${label}. Its microVM goes away; the worktree is kept so it can be resumed.`,
        resume_colony: `Resume colony ${args.id}${label}. It boots again on its kept worktree.`,
        move_to_front: `Move colony ${args.id}${label} to the front of the start queue.`,
        move_to_back: `Move colony ${args.id}${label} to the back of the start queue.`,
        publish_colony: `Open the pull request for colony ${args.id}${label}.`,
        launch_colony: `Start a colony on ${String(args.repo)} for ${args.issue ? `issue #${String(args.issue)}` : "a task"}.`,
        apply_update: "Install the newer Colonizer release.",
      };
      approval = {
        id, chat, message, tool, args, status: "pending", created_at: new Date().toISOString(),
        preview: {
          summary: summaries[tool] ?? `Run ${tool}.`,
          diff: [],
          dry_run: false,
          blast: tool === "launch_colony" ? { colonies: 1, orgs: 1, repos: 1, note: "Uses a slot and model spend." } : tool === "apply_update" ? { colonies: 0, orgs: 0, repos: 0, note: "The mothership drains, installs and restarts." } : { ...base, note: tool.startsWith("move_") ? "Changes who starts next." : "One colony." },
        },
      };
    }
    ms.mockApprovals.set(id, approval);
    return approval;
  };
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
    // The Colonizer tools (#1217): a read runs at once, a write is held for approval.
    const asked = c.messages.at(-1)?.content ?? "";
    const tools: ChatToolNote[] = [];
    let toolReply: string | null = null;
    const messageId = ms.mockId();
    if (lane === undefined && c.messages.at(-1)?.role === "user" && c.meta.model.includes("/")) {
      const live = [...ms.sessions.values()].map((m) => m.session);
      const running = live.filter((x) => x.status === "running");
      if (/\b(switch|move|change)\b.*\b(model|byteplus|minimax|glm)\b|\bbyteplus\b/i.test(asked)) {
        const approval = ms.mockPropose("switch_models", { scope: "install", roles: { subagent_model: "byteplus/glm-5.1" }, apply: "running" }, c.meta.id, messageId);
        tools.push({ tool: "model_assignments", kind: "read", status: "ran", summary: "Read the model assignments" });
        tools.push({ tool: "switch_models", kind: "write", status: "pending", summary: approval.preview.summary, approval: approval.id });
        toolReply = "I checked the current assignments. The subagent model is still on MiniMax; here is the switch to BytePlus, held for your approval. Nothing changes until you approve it.";
      } else if (/\bstop\b/i.test(asked) && running[0]) {
        const approval = ms.mockPropose("stop_colony", { id: running[0].id }, c.meta.id, messageId);
        tools.push({ tool: "stop_colony", kind: "write", status: "pending", summary: approval.preview.summary, approval: approval.id });
        toolReply = `That is colony ${running[0].id}. I have proposed stopping it; it will only stop if you approve.`;
      } else if (/(what|which|how many).*(running|colonies)|status/i.test(asked)) {
        tools.push({ tool: "list_colonies", kind: "read", status: "ran", summary: "Listed colonies", result: `${live.length} colonies` });
        toolReply = `${running.length} ${running.length === 1 ? "colony is" : "colonies are"} running right now${running[0] ? `, led by **${running[0].repo}**` : ""}; ${live.filter((x) => x.status === "queued").length} are queued. I only read this, nothing changed.`;
      }
    }
    const final = toolReply ?? answer;
    for (const note of tools) {
      const approval = note.approval ? ms.mockApprovals.get(note.approval) : undefined;
      onEvent(tag({ type: "tool" as const, note, approval }));
    }
    let text = "";
    let first: number | undefined;
    for (const word of final.split(/(?<= )/)) {
      if (signal?.aborted) break;
      await sleep(lane === 1 ? 45 : 30);
      first ??= Date.now() - started;
      text += word;
      onEvent(tag({ type: "delta" as const, text: word }));
    }
    const message: ChatMessage = { id: messageId, role: "assistant", content: text, ts: new Date().toISOString(), model, input_tokens: 120, output_tokens: 40, cost_usd: 0.0004, stopped: Boolean(signal?.aborted), parent_id: parent, first_token_ms: first, latency_ms: Date.now() - started, candidate: lane !== undefined || undefined, lane, tools: tools.length ? tools : undefined };
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
