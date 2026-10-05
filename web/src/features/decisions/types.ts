// The decisions inbox (issue #1036): repo decisions and pull requests that need a person, served by
// GET /api/decisions. Decisions are not colony questions; they share the inbox under their own header.

/** An open issue waiting on the operator's decision. */
export interface DecisionCard {
  /** `owner/repo#n`. */
  id: string;
  org: string;
  repo: string;
  number: number;
  title: string;
  url: string;
  question: string;
  /** Parsed from an "Options:" list; empty means the card offers free text only. */
  options: string[];
  /** Where the question was found. */
  source: "label" | "body" | "comment";
  /** The issue carries `needs-decision`, which an answer removes. */
  labelled: boolean;
  /** Further "Open decision:" lines in the same text. */
  more: number;
  updated_at: string | null;
}

/** Why a pull request needs a person, in the order the inbox lists them. */
export type PrReason = "commits_not_merged" | "policy_hold" | "needs_redo" | "conflicted" | "red_ci" | "review_requested" | "awaiting_merge";

export type PrAction = "rerun" | "redo" | "dismiss";

/** A pull request (or, held before publishing, a colony) that needs a person. */
export interface PrCard {
  /** The pull request URL, or `colony:<id>`. */
  id: string;
  org: string;
  repo: string;
  number: number | null;
  title: string;
  url: string | null;
  colony: string | null;
  reason: PrReason;
  /** Why it needs a person, in a sentence. */
  why: string;
  actions: PrAction[];
}

export interface DecisionOrg {
  org: string;
  enabled: boolean;
  default_on: boolean;
  explicit: boolean;
  polled_at: string | null;
  error: string | null;
}

export interface DecisionsView {
  /** Every card: decisions plus pull requests. */
  count: number;
  decisions: DecisionCard[];
  prs: PrCard[];
  orgs: DecisionOrg[];
  /** COLONIZER_NO_EXTERNAL_EFFECTS: read-only, answers and actions off. */
  writes_blocked: boolean;
  writes_blocked_reason: string | null;
  /** GitHub pushed back; searching resumes at this time. */
  paused_until: string | null;
  poll_minutes: number;
}

export interface DecisionAnswerRequest {
  id: string;
  choice: string;
  note?: string;
}

export interface DecisionAnswerReply {
  id: string;
  comment: string;
  label_removed: boolean;
  label_error: string | null;
}

export interface PrActionRequest {
  id: string;
  action: PrAction;
}

export interface PrActionReply {
  /** The Actions runs whose failed jobs re-ran. */
  rerun?: number[];
  /** The redo colony dispatched. */
  colony?: string;
  /** The commits-not-merged card dismissed (issue #1075). */
  dismissed?: string;
}
