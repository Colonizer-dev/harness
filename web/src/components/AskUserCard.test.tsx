// The answered ask_user card's origins (issue #312): the autonomy judge's answer must never read as
// the operator's own words. Rendered to static markup: the test environment has no DOM.
import type { ToolCallMessagePartProps } from "@assistant-ui/react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { AskUserArgs, AskUserResult } from "../sessionStream";
import type { Question } from "../types";
import { AskUserCard } from "./AskUserCard";

const questions: Question[] = [
  { question: "Push now?", header: "Push", multi_select: false, options: [{ label: "Yes" }] },
];

const card = (result: AskUserResult): string =>
  renderToStaticMarkup(
    <AskUserCard {...({ toolCallId: "q1", args: { questions }, result } as ToolCallMessagePartProps<AskUserArgs, AskUserResult>)} />,
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
