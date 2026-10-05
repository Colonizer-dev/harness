// The decisions inbox's API methods (issue #1036). The root `Api` interface composes `DecisionsApi`.
import { enc, post, put, request } from "../../http";
import type { DecisionAnswerReply, DecisionAnswerRequest, DecisionsView, PrActionReply, PrActionRequest } from "./types";

export interface DecisionsApi {
  /** GET /api/decisions: the decision and pull-request cards, the org opt-ins and the poll's state. */
  decisions(): Promise<DecisionsView>;
  /** POST /api/decisions/answer: posts one "Decision (maintainer):" comment and removes `needs-decision`. 409 while writes are blocked. */
  answerDecision(body: DecisionAnswerRequest): Promise<DecisionAnswerReply>;
  /** POST /api/decisions/pr-action: re-run a red pull request's failed jobs, or dispatch a redo colony. */
  decisionPrAction(body: PrActionRequest): Promise<PrActionReply>;
  /** PUT /api/decisions/orgs/{org}: opt an org in or out; `null` returns it to its default. */
  setDecisionOrg(org: string, enabled: boolean | null): Promise<DecisionsView>;
}

export const decisionsHttp: DecisionsApi = {
  decisions: () => request("/api/decisions"),
  answerDecision: (body) => post("/api/decisions/answer", body),
  decisionPrAction: (body) => post("/api/decisions/pr-action", body),
  setDecisionOrg: (org, enabled) => put(`/api/decisions/orgs/${enc(org)}`, { enabled }),
};
