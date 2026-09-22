// The inspector's question card: a waiting colony's question is readable and answerable from the
// pane — text, options, who asked and how long it has been waiting — and the pane says so instead
// of going blank when the stream has not delivered a payload yet. Answers flow from the live stream
// state, so an answered question drops out on its own. Rendered through react-dom/server, because
// this codebase keeps tests off jsdom.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { QuestionActions } from "../components/AskUserCard";
import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import { initialStreamState, reduceFrame, type StreamState } from "../sessionStream";
import type { ServerFrame, Session } from "../types";
import { Inspector, pendingQuestionsOf, type PendingQuestion } from "./Inspector";

const api = createMockApi();

const ASKED_AT = "2026-09-01T09:00:00Z";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails for guest users",
    status: "waiting_for_answer",
    branch: "colonizer/issue-42-s1",
    base: "main",
    parent: null,
    worktree: "/wt/s1",
    git_admin_dir: "/git/s1",
    sandbox: "colony-s1",
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false, keep_worktree: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

const noop = () => {};

// The stream is open and the colony is live: the card's submit is enabled and nothing is blocked.
const CONNECTED: QuestionActions = { answer: () => true, submitting: {}, canAnswer: true, blockedBy: null };
// The stream is still finding the colony: nothing can be answered yet.
const CONNECTING: QuestionActions = { answer: () => false, submitting: {}, canAnswer: false, blockedBy: "disconnected" };

function renderInspector(props: {
  pendingQuestions?: PendingQuestion[];
  questionActions?: QuestionActions;
  session?: Session;
}): string {
  const selected = props.session ?? session();
  return renderToStaticMarkup(
    <ApiContext.Provider value={api}>
      <Inspector
        target={{ kind: "colony", session: selected }}
        avatarUrl={null}
        settlers={[]}
        pendingQuestions={props.pendingQuestions ?? []}
        questionActions={props.questionActions ?? CONNECTED}
        sessions={[selected]}
        status={null}
        liveCount={0}
        queuedCount={0}
        needCount={1}
        spend={null}
        maxParallel={null}
        update={null}
        onClose={noop}
        onOpenColony={noop}
        onStop={noop}
        onResume={noop}
        onLaunch={noop}
        onOpenSettings={noop}
      />
    </ApiContext.Provider>,
  );
}

const PENDING: PendingQuestion = {
  id: "q1",
  questions: [
    { question: "Push the branch?", header: "Push", multi_select: false, options: [{ label: "Push now" }, { label: "Wait" }] },
  ],
  asked_at: ASKED_AT,
};

describe("Inspector", () => {
  it("shows a waiting colony's question, its options, who asked, and how long it has waited", () => {
    const markup = renderInspector({ pendingQuestions: [PENDING] });
    expect(markup).toContain("Push the branch?");
    expect(markup).toContain("Push now");
    expect(markup).toContain("Wait");
    // The pane's own header names the colony that asked.
    expect(markup).toContain("acme/webshop#42");
    // The frame timestamp lands as a relative duration, "asked 18d ago"-shaped.
    expect(markup).toMatch(/asked (just now|\d+m ago|\d+h ago|\d+d ago)/);
    // The way deeper is kept next to the in-pane card.
    expect(markup).toContain("open the colony and answer →");
  });

  it("without a question payload a waiting colony gets a labelled state, not a blank box", () => {
    const waiting = renderInspector({ pendingQuestions: [], questionActions: CONNECTED });
    expect(waiting).toContain("this colony has no pending question");

    const connecting = renderInspector({ pendingQuestions: [], questionActions: CONNECTING });
    expect(connecting).toContain("loading the question…");
  });

  it("nothing selected is its own labelled state", () => {
    const markup = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <Inspector
          target={null}
          avatarUrl={null}
          settlers={[]}
          pendingQuestions={[]}
          questionActions={CONNECTING}
          sessions={[]}
          status={null}
          liveCount={0}
          queuedCount={0}
          needCount={0}
          spend={null}
          maxParallel={null}
          update={null}
          onClose={noop}
          onOpenColony={noop}
          onStop={noop}
          onResume={noop}
          onLaunch={noop}
          onOpenSettings={noop}
        />
      </ApiContext.Provider>,
    );
    expect(markup).toMatch(/aria-label="nothing selected"/);
    expect(markup).toContain("nothing selected");
  });
});

// The BOOT section (issue #360): where a colony's launch time went, read from `boot_timing`.
describe("Inspector boot timing", () => {
  const PHASES = [
    { name: "issue", ms: 240 },
    { name: "git", ms: 1_180 },
    { name: "mesh-start", ms: 2_050 },
    { name: "vm-boot", ms: 86_400 },
    { name: "agentd", ms: 720 },
  ];

  it("a booted colony lists its phases and marks the slowest", () => {
    const markup = renderInspector({ session: session({ status: "running", boot_timing: { total_ms: 94_320, phases: PHASES } }) });
    expect(markup).toContain(">BOOT<");
    for (const name of ["issue", "git", "mesh-start", "vm-boot", "agentd"]) expect(markup).toContain(`>${name}<`);
    expect(markup).toContain("240 ms");
    // Exactly one marker, on the vm-boot row.
    expect(markup.match(/SLOWEST/g)).toHaveLength(1);
    expect(markup).toMatch(/>vm-boot<[^]*?SLOWEST[^]*?1m 26s/);
    expect(markup).toContain("total 94s");
  });

  it("a starting colony shows the last phase it finished", () => {
    const markup = renderInspector({
      session: session({ status: "starting", boot_timing: { phases: PHASES.slice(0, 3) } }),
    });
    expect(markup).toContain("starting · last done: mesh-start");
    expect(markup).not.toContain(">vm-boot<");
  });

  it("a colony whose boot stopped part way says where, not a partial total", () => {
    const markup = renderInspector({ session: session({ status: "failed", boot_timing: { phases: PHASES.slice(0, 3) } }) });
    expect(markup).toContain("stopped after mesh-start");
    expect(markup).not.toContain("total ");
  });

  it("a starting colony before its first phase says so", () => {
    const markup = renderInspector({ session: session({ status: "starting", boot_timing: null }) });
    expect(markup).toContain("starting · no phase finished yet");
  });

  it("a colony without boot timing has no BOOT section", () => {
    const markup = renderInspector({ session: session({ status: "running" }) });
    expect(markup).not.toContain(">BOOT<");
  });
});

describe("pendingQuestionsOf", () => {
  const ASK = { question: "Push now?", header: "Push", multi_select: false, options: [{ label: "Yes" }] };
  const frame = (body: ServerFrame): ServerFrame => body;

  /** A stream where one question was asked, answered or not, on the wire itself. */
  const streamWithQuestion = (answered: boolean): StreamState => {
    let state = reduceFrame(
      initialStreamState(),
      frame({ type: "assistant_text", seq: 1, ts: ASKED_AT, message_id: "m1", block_index: 0, text: "Shall I?" }),
    );
    state = reduceFrame(state, frame({ type: "question", seq: 2, ts: ASKED_AT, question_id: "q1", message_id: "m1", questions: [ASK] }));
    if (answered) {
      state = reduceFrame(
        state,
        frame({ type: "question_answered", seq: 3, ts: ASKED_AT, question_id: "q1", answers: { "Push now?": "Yes" }, response: null }),
      );
    }
    return state;
  };

  it("lists only the questions still waiting for an answer", () => {
    expect(pendingQuestionsOf(streamWithQuestion(false))).toEqual([{ id: "q1", questions: [ASK], asked_at: ASKED_AT }]);
  });

  it("an answered question drops out, so the pane clears as question_answered arrives", () => {
    expect(pendingQuestionsOf(streamWithQuestion(true))).toEqual([]);
  });
});