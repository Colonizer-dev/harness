// The decisions inbox's mock methods (issue #1036): demo decision and pull-request cards — one of
// each reason — served through the same components the real inbox uses, so `npm run build:demo`
// shows them. Answering drops the card, as the mothership does once the comment is posted.
import { ApiError } from "../../http";
import { ago, clone, sleep } from "../../mockShared";
import type { MockState } from "../../mockState";
import type { DecisionsApi } from "./api";
import type { DecisionCard, DecisionsView, PrCard } from "./types";

/** The demo's open decisions: one with options parsed from the issue, one free-text only. */
export function demoDecisions(): DecisionCard[] {
  return [
    {
      id: "acme/webshop#58",
      org: "acme",
      repo: "acme/webshop",
      number: 58,
      title: "Cart: where should saved carts live?",
      url: "https://github.com/acme/webshop/issues/58",
      question: "Should saved carts live in the session store or in Postgres?",
      options: ["Session store (expires with the session)", "Postgres (kept for 30 days)", "Both: session first, Postgres on checkout"],
      source: "body",
      labelled: true,
      more: 0,
      updated_at: ago(40),
    },
    {
      id: "acme/design-system#21",
      org: "acme",
      repo: "acme/design-system",
      number: 21,
      title: "Rename the `subtle` button variant",
      url: "https://github.com/acme/design-system/issues/21",
      question: "What should the `subtle` button variant be called?",
      options: [],
      source: "comment",
      labelled: false,
      more: 0,
      updated_at: ago(180),
    },
  ];
}

/** The demo's pull requests that need a person: one card per reason the inbox knows. */
export function demoPrCards(): PrCard[] {
  return [
    {
      id: "colony:stuck2468",
      org: "acme",
      repo: "acme/webshop",
      number: null,
      title: "Add dark mode to the order confirmation email",
      url: null,
      colony: "stuck2468",
      reason: "policy_hold",
      why: "a secret was redacted from its pull request description; the publish waits until someone has looked",
      actions: [],
    },
    {
      id: "https://github.com/acme/webshop/pull/61",
      org: "acme",
      repo: "acme/webshop",
      number: 61,
      title: "Price rounding in cart totals",
      url: "https://github.com/acme/webshop/pull/61",
      colony: "old98765",
      reason: "conflicted",
      why: "it is behind or conflicts with its base, and the auto-rebase could not finish",
      actions: ["redo"],
    },
    {
      id: "https://github.com/acme/webshop/pull/66",
      org: "acme",
      repo: "acme/webshop",
      number: 66,
      title: "Rate-limit the checkout API",
      url: "https://github.com/acme/webshop/pull/66",
      colony: null,
      reason: "red_ci",
      why: "CI is red and not a known flake: checks failing: test (ubuntu-latest)",
      actions: ["rerun"],
    },
    {
      id: "https://github.com/acme/design-system/pull/24",
      org: "acme",
      repo: "acme/design-system",
      number: 24,
      title: "Token docs: dark-mode examples",
      url: "https://github.com/acme/design-system/pull/24",
      colony: null,
      reason: "review_requested",
      why: "dana asked you for a review",
      actions: [],
    },
  ];
}

export function decisionsMock(ms: MockState): DecisionsApi {
  void ms;
  const state: DecisionsView = {
    count: 0,
    decisions: demoDecisions(),
    prs: demoPrCards(),
    orgs: [
      { org: "acme", enabled: true, default_on: true, explicit: false, polled_at: ago(2), error: null },
      { org: "octocat", enabled: false, default_on: false, explicit: false, polled_at: null, error: null },
    ],
    writes_blocked: false,
    writes_blocked_reason: null,
    paused_until: null,
    poll_minutes: 5,
  };
  const view = (): DecisionsView => {
    const on = new Set(state.orgs.filter((o) => o.enabled).map((o) => o.org));
    const decisions = state.decisions.filter((d) => on.has(d.org));
    const prs = state.prs.filter((p) => on.has(p.org));
    return clone({ ...state, decisions, prs, count: decisions.length + prs.length });
  };
  return {
    decisions: async () => {
      await sleep(120);
      return view();
    },
    answerDecision: async (body) => {
      await sleep(300);
      const card = state.decisions.find((d) => d.id === body.id);
      if (!card) throw new ApiError("no open decision with that id", 404);
      if (!body.choice.trim()) throw new ApiError("pick an option or write an answer", 400);
      state.decisions = state.decisions.filter((d) => d.id !== body.id);
      const note = body.note?.trim();
      return {
        id: card.id,
        comment: `Decision (maintainer): ${body.choice.trim()}${note ? `\n\n${note}` : ""}`,
        label_removed: card.labelled,
        label_error: null,
      };
    },
    decisionPrAction: async (body) => {
      await sleep(300);
      const card = state.prs.find((p) => p.id === body.id);
      if (!card) throw new ApiError("no open pull-request card with that id", 404);
      if (!card.actions.includes(body.action)) throw new ApiError(`"${body.action}" is not an action this card offers`, 400);
      if (body.action === "redo") {
        state.prs = state.prs.filter((p) => p.id !== body.id);
        return { colony: "redo1234" };
      }
      return { rerun: [9_120_334_571] };
    },
    setDecisionOrg: async (org, enabled) => {
      await sleep(150);
      const entry = state.orgs.find((o) => o.org === org.toLowerCase());
      if (entry) {
        entry.enabled = enabled ?? entry.default_on;
        entry.explicit = enabled !== null;
      }
      return view();
    },
  };
}
