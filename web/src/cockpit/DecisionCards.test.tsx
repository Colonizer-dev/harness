// The decisions inbox (issue #1036): a repo decision renders like a colony question card and sends
// exactly the answer the form holds; a pull-request card says why, links to the pull request and
// offers only its own quick actions; with writes blocked everything is off and says why; and the
// cards join the one "need you" count without counting a colony twice. Rendered to static markup
// (the test environment has no DOM), so the actions are pinned through the pure helpers.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type { DecisionsView, PrCard, Session } from "../types";
import { demoDecisions, demoPrCards } from "../features/decisions/mock";
import {
  DecisionCardView,
  DecisionsSection,
  OTHER,
  PrCardView,
  decisionAnswer,
  prActionSummary,
  runDecisionAnswer,
  runPrAction,
} from "./DecisionCards";
import { decisionsCount, prCardColonyIds } from "./decisions";
import { InboxView } from "./InboxView";

const [withOptions, freeText] = demoDecisions();
const prs = demoPrCards();
const pr = (reason: PrCard["reason"]) => prs.find((p) => p.reason === reason)!;

function view(overrides: Partial<DecisionsView> = {}): DecisionsView {
  return {
    count: 2 + prs.length,
    decisions: [withOptions, freeText],
    prs,
    orgs: [],
    writes_blocked: false,
    writes_blocked_reason: null,
    paused_until: null,
    poll_minutes: 5,
    ...overrides,
  };
}

const session = (id: string, extra: Partial<Session> = {}) =>
  ({ id, repo: "acme/webshop", org: "acme", issue: 1, issue_title: "x", status: "running", attention: null, ...extra }) as unknown as Session;

describe("decision answers", () => {
  it("send the picked option, or the free text, with the note trimmed and nothing while incomplete", () => {
    expect(decisionAnswer(withOptions, null, "", "")).toBeNull();
    expect(decisionAnswer(withOptions, withOptions.options[1], "", "  ")).toEqual({ id: withOptions.id, choice: withOptions.options[1] });
    expect(decisionAnswer(withOptions, OTHER, "  Redis  ", " cheaper ")).toEqual({ id: withOptions.id, choice: "Redis", note: "cheaper" });
    expect(decisionAnswer(withOptions, OTHER, "   ", "")).toBeNull();
    // No options: the card is free text only.
    expect(decisionAnswer(freeText, null, "quiet", "")).toEqual({ id: freeText.id, choice: "quiet" });
  });

  it("render like a question card: the question, every option, Other and the post button", () => {
    const html = renderToStaticMarkup(<DecisionCardView card={withOptions} blocked={null} onAnswer={async () => null} />);
    expect(html).toContain("A decision for you");
    expect(html).toContain(withOptions.question);
    for (const option of withOptions.options) expect(html).toContain(option);
    expect(html).toContain("Other…");
    expect(html).toContain("Post decision");
    expect(html).toContain(`href="${withOptions.url}"`);
    expect(html).toContain("acme/webshop#58");
    expect(html).toContain("needs-decision");
  });

  it("offer free text when the issue lists no options", () => {
    const html = renderToStaticMarkup(<DecisionCardView card={freeText} blocked={null} onAnswer={async () => null} />);
    expect(html).toContain("Your decision");
    expect(html).not.toContain("Other…");
  });

  it("are off, and say why, while external writes are blocked", () => {
    const html = renderToStaticMarkup(
      <DecisionCardView card={withOptions} blocked="COLONIZER_NO_EXTERNAL_EFFECTS is set" onAnswer={async () => null} />,
    );
    expect(html).toContain("Answering is off: COLONIZER_NO_EXTERNAL_EFFECTS is set.");
    expect(html).toMatch(/<button[^>]*type="submit"[^>]*disabled=""/);
  });

  it("post through the API once and say what happened", async () => {
    const send = vi.fn(async () => ({ id: withOptions.id, comment: "Decision (maintainer): A", label_removed: true, label_error: null }));
    const say = vi.fn();
    await runDecisionAnswer(send, say, { id: withOptions.id, choice: "A" });
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith({ id: withOptions.id, choice: "A" });
    expect(say).toHaveBeenCalledWith("Decision posted on acme/webshop#58 and needs-decision removed");

    const failing = vi.fn(async () => {
      throw new Error("COLONIZER_NO_EXTERNAL_EFFECTS is set");
    });
    expect(await runDecisionAnswer(failing, say, { id: withOptions.id, choice: "A" })).toBeNull();
    expect(say).toHaveBeenLastCalledWith("COLONIZER_NO_EXTERNAL_EFFECTS is set", "error");
  });
});

describe("pull-request cards", () => {
  it("say why, link to the pull request and offer only their own actions", () => {
    const red = renderToStaticMarkup(<PrCardView card={pr("red_ci")} blocked={null} onAction={async () => null} onOpenColony={() => {}} />);
    expect(red).toContain("CI red");
    expect(red).toContain("Why it needs you: CI is red and not a known flake");
    expect(red).toContain("Re-run failed jobs");
    expect(red).not.toContain("Dispatch redo colony");
    expect(red).toContain(`href="${pr("red_ci").url}"`);
    expect(red).toContain("Open on GitHub");

    const conflicted = renderToStaticMarkup(<PrCardView card={pr("conflicted")} blocked={null} onAction={async () => null} onOpenColony={() => {}} />);
    expect(conflicted).toContain("Dispatch redo colony");
    expect(conflicted).toContain("Open colony");

    const review = renderToStaticMarkup(<PrCardView card={pr("review_requested")} blocked={null} onAction={async () => null} onOpenColony={() => {}} />);
    expect(review).toContain("Review requested");
    expect(review).not.toContain("Re-run failed jobs");

    // Held before publishing: no pull request to link to, the colony is the way in.
    const held = renderToStaticMarkup(<PrCardView card={pr("policy_hold")} blocked={null} onAction={async () => null} onOpenColony={() => {}} />);
    expect(held).toContain("Held by policy");
    expect(held).not.toContain("Open on GitHub");
    expect(held).toContain("Open colony");
  });

  it("turn their actions off while writes are blocked", () => {
    const html = renderToStaticMarkup(<PrCardView card={pr("red_ci")} blocked="read-only" onAction={async () => null} onOpenColony={() => {}} />);
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>.*Re-run failed jobs/);
  });

  it("send the card's id and action, and say what happened", async () => {
    const say = vi.fn();
    const rerun = vi.fn(async () => ({ rerun: [11, 12] }));
    await runPrAction(rerun, say, pr("red_ci"), "rerun");
    expect(rerun).toHaveBeenCalledWith({ id: pr("red_ci").id, action: "rerun" });
    expect(say).toHaveBeenCalledWith("Re-running the failed jobs of 2 runs");
    const redo = vi.fn(async () => ({ colony: "redo1234" }));
    await runPrAction(redo, say, pr("conflicted"), "redo");
    expect(say).toHaveBeenLastCalledWith("Redo colony redo1234 dispatched");
    expect(prActionSummary("rerun", { rerun: [1] })).toBe("Re-running the failed jobs of 1 run");
    const gone: PrCard = { ...pr("red_ci"), reason: "commits_not_merged", actions: ["dismiss"] };
    const dismiss = vi.fn(async () => ({ dismissed: gone.id }));
    await runPrAction(dismiss, say, gone, "dismiss");
    expect(dismiss).toHaveBeenCalledWith({ id: gone.id, action: "dismiss" });
    expect(say).toHaveBeenLastCalledWith("Dismissed");
  });
});

describe("the inbox", () => {
  it("counts every card once, leaving out a colony that already needs you", () => {
    const sessions = [session("stuck2468", { attention: { reason: "autopilot_held" } as Session["attention"] }), session("old98765")];
    // stuck2468 already counts as a colony; its policy card does not count again.
    expect(decisionsCount(view(), sessions)).toBe(2 + prs.length - 1);
    expect(decisionsCount(null, sessions)).toBe(0);
    expect(prCardColonyIds(view())).toEqual(new Set(["stuck2468", "old98765"]));
  });

  it("shows the decisions under their own header, in the same count, and a held colony only once", () => {
    const sessions = [session("stuck2468", { attention: { reason: "autopilot_held" } as Session["attention"] })];
    const html = renderToStaticMarkup(
      <InboxView sessions={sessions} onOpenColony={() => {}} onOpenNotificationSettings={() => {}} decisions={view()} />,
    );
    expect(html).toContain(`${1 + 2 + prs.length - 1} need you`);
    expect(html).toContain(">Decisions<");
    expect(html).toContain("not colony questions");
    expect(html).toContain("Pull requests that need you");
    expect(html).toContain(withOptions.question);
    expect(html).not.toContain("nothing waits on you");
    // The held colony is on its policy card, not listed again as a question.
    expect(html).not.toContain("the colony asked you a question");
  });

  it("shows nothing extra without cards, and a read-only note when writes are blocked", () => {
    const empty = renderToStaticMarkup(<DecisionsSection view={view({ decisions: [], prs: [] })} onAnswer={async () => null} onPrAction={async () => null} onOpenColony={() => {}} />);
    expect(empty).toBe("");
    const blocked = renderToStaticMarkup(
      <DecisionsSection
        view={view({ writes_blocked: true, writes_blocked_reason: "COLONIZER_NO_EXTERNAL_EFFECTS is set" })}
        onAnswer={async () => null}
        onPrAction={async () => null}
        onOpenColony={() => {}}
      />,
    );
    expect(blocked).toContain("Read-only: COLONIZER_NO_EXTERNAL_EFFECTS is set.");
  });
});
