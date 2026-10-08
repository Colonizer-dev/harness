import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { ChatApproval, ChatToolNote } from "../../types";
import { ApprovalCard, ToolCalls, type ApprovalsHandle } from "./ToolCalls";

const approval = (over: Partial<ChatApproval> = {}): ChatApproval => ({
  id: "a1",
  chat: "c",
  message: "m",
  tool: "switch_models",
  args: {},
  status: "pending",
  created_at: "",
  preview: {
    summary: "Switch the subagent model to byteplus/glm-5.1 for all orgs.",
    diff: [{ scope: "install", target: "agent", key: "subagent_model", was: "minimax/m", now: "byteplus/glm-5.1" }],
    dry_run: true,
    blast: { colonies: 7, orgs: 3, repos: 5, note: "7 running colonies restart." },
  },
  ...over,
});

describe("ApprovalCard", () => {
  it("shows the words, the diff and the blast radius, with Approve, Edit and Reject", () => {
    const html = renderToStaticMarkup(<ApprovalCard approval={approval()} onDecide={() => {}} />);
    expect(html).toContain("Needs your approval");
    expect(html).toContain("Switch the subagent model to byteplus/glm-5.1 for all orgs.");
    expect(html).toContain("Subagent model");
    expect(html).toContain("minimax/m");
    expect(html).toContain(">7</span>");
    expect(html).toContain("colonies");
    for (const label of ["Approve", "Edit", "Reject"]) expect(html).toContain(label);
    expect(html).toContain("Nothing runs until you approve.");
  });
  it("loses its buttons once settled and says what ran", () => {
    const done = renderToStaticMarkup(<ApprovalCard approval={approval({ status: "approved", result: "Switched 3 settings." })} onDecide={() => {}} />);
    expect(done).not.toContain("Nothing runs until");
    expect(done).toContain("Approved");
    expect(done).toContain("Switched 3 settings.");
    expect(renderToStaticMarkup(<ApprovalCard approval={approval({ status: "rejected" })} onDecide={() => {}} />)).toContain("Rejected");
    expect(renderToStaticMarkup(<ApprovalCard approval={approval({ status: "failed", result: "409: boom" })} onDecide={() => {}} />)).toContain("it failed");
  });
});

describe("ToolCalls", () => {
  const handle = (list: ChatApproval[]): ApprovalsHandle => ({ byId: Object.fromEntries(list.map((a) => [a.id, a])), add: () => {}, decide: async () => null, busy: new Set() });
  it("draws reads as quiet notes and offers 'Approve all' for several held writes", () => {
    const notes: ChatToolNote[] = [
      { tool: "list_colonies", kind: "read", status: "ran", summary: "Listed colonies" },
      { tool: "stop_colony", kind: "write", status: "pending", summary: "s", approval: "a1" },
      { tool: "stop_colony", kind: "write", status: "pending", summary: "s", approval: "a2" },
    ];
    const html = renderToStaticMarkup(<ToolCalls notes={notes} approvals={handle([approval({ id: "a1", tool: "stop_colony" }), approval({ id: "a2", tool: "stop_colony" })])} />);
    expect(html).toContain("Listed colonies");
    expect(html).toContain("read only");
    expect(html).toContain("Approve all 2");
    expect(html.match(/Needs your approval/g)).toHaveLength(2);
  });
  it("says where a write stands when its card is not loaded", () => {
    const html = renderToStaticMarkup(<ToolCalls notes={[{ tool: "stop_colony", kind: "write", status: "rejected", summary: "Stop colony x", approval: "gone" }]} approvals={handle([])} />);
    expect(html).toContain("Rejected");
    expect(html).toContain("Stop colony x");
  });
});
