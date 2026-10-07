// The answered ask_user card's origins (issue #312): the autonomy judge's answer must never read as
// the operator's own words. Rendered to static markup: the test environment has no DOM. The open
// card's queueing state (issue #746) is pinned through the same context the panes provide.
import type { ToolCallMessagePartProps } from "@assistant-ui/react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { AskUserArgs, AskUserResult } from "../sessionStream";
import type { Question } from "../types";
import { AskUserCard, QuestionActionsContext, type QuestionActions } from "./AskUserCard";

const questions: Question[] = [
  { question: "Push now?", header: "Push", multi_select: false, options: [{ label: "Yes" }] },
];

const card = (result: AskUserResult): string =>
  renderToStaticMarkup(
    <AskUserCard {...({ toolCallId: "q1", args: { questions }, result } as ToolCallMessagePartProps<AskUserArgs, AskUserResult>)} />,
  );

const openCard = (actions: Partial<QuestionActions>): string =>
  renderToStaticMarkup(
    <QuestionActionsContext.Provider value={{ answer: () => true, submitting: {}, canAnswer: true, blockedBy: null, ...actions }}>
      <AskUserCard {...({ toolCallId: "q1", args: { questions }, result: undefined } as unknown as ToolCallMessagePartProps<AskUserArgs, AskUserResult>)} />
    </QuestionActionsContext.Provider>,
  );

describe("AnsweredCard origins", () => {
  it("an operator's answer reads as their own, in the plain style", () => {
    const out = card({ answers: { "Push now?": "Yes" }, response: null });
    expect(out).toContain("You answered");
    expect(out).toContain("text-muted");
    expect(out).not.toContain("autonomy judge");
  });

  it("a judge's answer is labelled and styled apart from the operator's", () => {
    const out = card({ answers: { "Push now?": "Yes" }, response: null, origin: "autonomy" });
    expect(out).toContain("Answered by the autonomy judge");
    expect(out).toContain("text-info");
    expect(out).not.toContain("You answered");
  });
});

describe("OpenCard queueing", () => {
  it("connected, it is a plain Submit answer", () => {
    const out = openCard({});
    expect(out).toContain("Submit answer");
    expect(out).toContain("Answer 1 more question to continue."); // nothing picked yet
    expect(out).not.toContain("Queue answer");
  });

  it("offline with the outbox behind it, the card stays open and owns the queue", () => {
    const out = openCard({ willQueue: true });
    expect(out).toContain("Queue answer");
    expect(out).toContain("your answer will queue and send when you"); // react escapes the apostrophe
    expect(out).not.toContain("Reconnecting");
  });

  it("offline without queueing, it is still the reconnecting wall", () => {
    const out = openCard({ canAnswer: false, blockedBy: "disconnected" });
    expect(out).toContain("Reconnecting to the colony…");
    expect(out).not.toContain("Queue answer");
  });
});
