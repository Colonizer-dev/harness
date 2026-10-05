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
import { loadSessionDiff } from "../sessionDiff";
import { initialStreamState, reduceFrame, type StreamState } from "../sessionStream";
import type { ServerFrame, Session, SessionDiffFile } from "../types";
import type { Api } from "../api";
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
  sessions?: Session[];
  api?: Api;
}): string {
  const selected = props.session ?? session();
  return renderToStaticMarkup(
    <ApiContext.Provider value={props.api ?? api}>
      <Inspector
        target={{ kind: "colony", session: selected }}
        avatarUrl={null}
        settlers={[]}
        pendingQuestions={props.pendingQuestions ?? []}
        questionActions={props.questionActions ?? CONNECTED}
        sessions={props.sessions ?? [selected]}
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

  // Issue #1093: World360-Lab#37's card said "the watchdog flagged this colony · this colony has no
  // pending question · open the colony and answer" over a colony held on a gateway error.
  it("a colony held on gateway errors is carded by its cause, with Retry and no question", () => {
    const markup = renderInspector({
      session: session({
        status: "idle",
        autopilot: true,
        attention: {
          reason: "autopilot_held",
          since: ASKED_AT,
          nudges: 0,
          cause: "gateway_error",
          detail: "Stopped on repeated gateway errors (502, connection to Anthropic); 3 automatic retries did not get through",
        },
      }),
    });
    expect(markup).toContain("Stopped on repeated gateway errors (502, connection to Anthropic)");
    expect(markup).toContain(">Retry<");
    expect(markup).toContain("Open colony");
    expect(markup).not.toContain("the watchdog flagged this colony");
    expect(markup).not.toContain("no pending question");
    expect(markup).not.toContain("answer");
  });

  it("a colony backing off an automatic retry is not waiting on you", () => {
    const markup = renderInspector({
      session: session({
        status: "parked",
        autopilot: true,
        attention: {
          reason: "provider_retry",
          since: ASKED_AT,
          nudges: 0,
          cause: "gateway_error",
          summary: "Stopped on a model gateway error (502, connection to Anthropic)",
          retry_at: new Date(Date.now() + 4 * 60_000 - 1_000).toISOString(),
          attempt: 1,
          max_attempts: 3,
        },
      }),
    });
    expect(markup).not.toContain("Waiting on you");
    expect(markup).toContain("Retrying automatically");
    expect(markup).toContain("Stopped on a model gateway error (502, connection to Anthropic): retrying in 4 min");
    expect(markup).toContain("Retry now");
    expect(markup).toContain("attempt 1 of 3");
    expect(markup).not.toContain("watchdog");
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

// The mothership pane's cross-colony medians: per-phase medians across recent finished boots.
describe("Inspector boot medians", () => {
  const renderMothership = (sessions: Session[]): string =>
    renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <Inspector target={{ kind: "mothership" }} avatarUrl={null} settlers={[]} pendingQuestions={[]}
          questionActions={CONNECTED} sessions={sessions} status={null} liveCount={0} queuedCount={0}
          needCount={0} spend={null} maxParallel={null} update={null} onClose={noop} onOpenColony={noop}
          onStop={noop} onResume={noop} onLaunch={noop} onOpenSettings={noop} />
      </ApiContext.Provider>,
    );

  const PHASES = [{ name: "git", ms: 1_180 }, { name: "vm-boot", ms: 86_400 }];

  it("shows per-phase medians and the median total across the sampled boots", () => {
    const markup = renderMothership([
      session({ id: "a", created_at: "2026-09-18T09:00:00Z", boot_timing: { total_ms: 90_000, phases: PHASES } }),
      session({ id: "b", created_at: "2026-09-18T09:01:00Z", boot_timing: { total_ms: 94_320, phases: PHASES } }),
    ]);
    expect(markup).toContain("BOOT · MEDIAN OF 2");
    expect(markup).toContain(">git<");
    expect(markup).toContain(">vm-boot<");
    expect(markup).toContain("median total 94s");
    expect(markup.match(/SLOWEST/g)).toHaveLength(1);
  });

  it("no finished boots is no section", () => {
    const unfinished = session({ id: "a", status: "starting", boot_timing: { phases: PHASES.slice(0, 1) } });
    expect(renderMothership([unfinished, session({ id: "b", status: "running" })])).not.toContain("MEDIAN OF");
  });
});

// The stuck-colony readout (issue #230): the single-session route's diagnosis and recent events.
describe("Inspector diagnosis", () => {
  const QUOTA = "API Error: quota has been exhausted. The quota will reset at 09-23 07:54:00 UTC.";
  const diagnosed = (overrides: Partial<Session> = {}) =>
    session({
      status: "running",
      diagnosis: { state: "waiting_on_provider", text: `waiting on provider: ${QUOTA}`, resets_at: "2026-09-23T07:54:00Z" },
      recent_events: [{ seq: 42, ts: "2026-09-18T08:20:00Z", type: "assistant_text", summary: QUOTA }],
      ...overrides,
    });

  it("shows the diagnosis text and the recent events of a stuck colony", () => {
    const markup = renderInspector({ session: diagnosed() });
    expect(markup).toContain("waiting on provider");
    expect(markup).toContain(QUOTA);
    expect(markup).toContain(">STATUS<");
    expect(markup).toContain(">RECENT EVENTS<");
    expect(markup).toContain("assistant_text");
  });

  it("a terminal session shows no diagnosis row, even carrying one", () => {
    const markup = renderInspector({ session: diagnosed({ status: "stopped" }) });
    expect(markup).not.toContain(">STATUS<");
    expect(markup).not.toContain(">RECENT EVENTS<");
    expect(markup).not.toContain(QUOTA);
  });
});

// The successor queue (issue #321): a queued `claim_wait` colony reads where in line it stands,
// counted oldest-first across the waiters of its issue.
describe("Inspector claim wait", () => {
  const waiter = (id: string, created_at: string): Session =>
    session({ id, status: "queued", claim_wait: true, queued_behind: "holder1", created_at });

  it("a second waiter shows its position, in the status line and the queued-behind fact", () => {
    const first = waiter("first", "2026-09-18T09:30:00Z");
    const second = waiter("second", "2026-09-18T10:00:00Z");
    const markup = renderInspector({ session: second, sessions: [first, second] });
    expect(markup).toContain("Queued behind holder1 · #2 in line");
    expect(markup).toContain("holder1 · #2 in line");
  });

  it("a plain queued colony names the colony it waits for and no line position", () => {
    const plain = session({ id: "plain", status: "queued", queued_behind: "holder1" });
    const markup = renderInspector({ session: plain, sessions: [plain] });
    expect(markup).toContain("Queued behind holder1");
    expect(markup).not.toContain("in line");
  });
});

// The pull request card's changed files (issue #611): the files behind a colony's PR, each with its
// +/- counts, folded after a few. The counts load through the cached client, so a test seeds that
// cache first — the server render runs no effects, and the cache is what the first paint reads.
describe("Inspector pull request files", () => {
  const FILES: SessionDiffFile[] = [
    { path: "web/src/cockpit/Inspector.tsx", added: 41, removed: 6 },
    { path: "web/src/api.ts", added: 8, removed: 0 },
    { path: "web/src/types.ts", added: 12, removed: 1 },
    { path: "web/src/cockpit/Inspector.test.tsx", added: 55, removed: 2 },
    { path: "docs/gaps.md", added: 1, removed: 1 },
    { path: "changelog.d/611.added.md", added: 4, removed: 0 },
  ];

  /** The mock client with /diff answering FILES, seeded through the same path the pane fetches on. */
  const diffApi: Api = {
    ...createMockApi(),
    sessionDiff: async () => ({ id: "s1", repo: "acme/webshop", base: "main", files: FILES, added: 121, removed: 10, diff: "", truncated: false }),
  };

  it("lists the changed files with +/- counts, folding the list after a few", async () => {
    await loadSessionDiff(diffApi, "s1", "2026-09-18T09:10:00Z");
    const markup = renderInspector({
      api: diffApi,
      session: session({ pr_url: "https://github.com/acme/webshop/pull/42", publish_stage: "pr_opened" }),
    });
    // The first files and their counts.
    expect(markup).toContain("web/src/cockpit/Inspector.tsx");
    expect(markup).toContain("+41");
    expect(markup).toContain("-6");
    // Six files, five shown: the sixth folds behind "+1 more", a disclosure the toggle announces.
    expect(markup).toContain("+1 more");
    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain("aria-controls=");
    expect(markup).not.toContain("changelog.d/611.added.md");
  });

  it("a colony without a pull request has no file rows", () => {
    const markup = renderInspector({ api: diffApi, session: session({ pr_url: null }) });
    expect(markup).not.toContain("web/src/cockpit/Inspector.tsx");
    expect(markup).not.toContain("+41");
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