// The `queues` feature's mock methods and fixtures (issue #1127). The fixture is stateless — the
// page's bulk actions go through the sessions and host mocks' own resume/stop/restart, so there is
// no queue state to keep — so this slice, like builtWith, installs no mockState.
import { ago, clone } from "../../mockShared";
import type { MockState } from "../../mockState";
import type { QueuesApi } from "./api";
import { repoKey } from "./model";
import type { QueuesFilters, QueuesPayload, QueuesRow } from "./types";

/** `ahead` in mockShared counts days; the queue's resumes land within hours. */
const aheadMins = (minutes: number) => new Date(Date.now() + minutes * 60_000).toISOString();

/** The busy install the Queues page draws with `?mock=1`: rows across the wait reasons, two hosts. */
export const queuesSample: QueuesPayload = {
  rows: [
    {
      id: "q_demo0001",
      org: "acme",
      repo: "acme/webshop",
      issue: 42,
      title: "Checkout fails for guest users",
      branch: "colonizer/issue-42-q_demo0001",
      host: "build-box",
      agent: "claude-code",
      status: "queued",
      created_at: ago(4),
      priority: 0,
      reason: null,
      detail: null,
      attention: null,
      resumes_at: null,
      held: false,
      policy_hold: false,
      actions: { resume: false, stop: true, restart: false },
      why_not: { resume: "only parked colonies can be resumed", restart: "already on the current version" },
    },
    {
      id: "q_demo0002",
      org: "acme",
      repo: "acme/api",
      issue: 88,
      title: "Idempotency keys on refunds",
      branch: "colonizer/issue-88-q_demo0002",
      host: "build-box",
      agent: "claude-code",
      status: "parked",
      created_at: ago(26),
      priority: 0,
      reason: "provider_quota_exhausted",
      detail: null,
      attention: null,
      resumes_at: aheadMins(180),
      held: false,
      policy_hold: false,
      actions: { resume: true, stop: true, restart: false },
      why_not: { restart: "already on the current version" },
    },
    {
      id: "q_demo0003",
      org: "acme",
      repo: "acme/webshop",
      issue: 61,
      title: "Price rounding in cart totals",
      branch: "colonizer/issue-61-q_demo0003",
      host: "build-box",
      agent: "claude-code",
      status: "parked",
      created_at: ago(52),
      priority: 0,
      reason: "repo_pr_rate_limit",
      detail: "the repository's daily pull-request cap is spent; it resumes at the next UTC day",
      attention: null,
      resumes_at: aheadMins(8 * 60),
      held: false,
      policy_hold: false,
      actions: { resume: true, stop: true, restart: false },
      why_not: { restart: "already on the current version" },
    },
    {
      id: "q_demo0004",
      org: "acme",
      repo: "acme/design-system",
      issue: 7,
      title: "Bad contrast on the nav",
      branch: "colonizer/issue-7-q_demo0004",
      host: "build-box",
      agent: "claude-code",
      status: "waiting_for_answer",
      created_at: ago(9),
      priority: 0,
      reason: "waiting_for_answer",
      detail: null,
      attention: { reason: "waiting_for_answer", since: ago(9), nudges: 0 },
      resumes_at: null,
      held: false,
      policy_hold: false,
      actions: { resume: false, stop: true, restart: false },
      why_not: { resume: "only parked colonies can be resumed", restart: "already on the current version" },
    },
    {
      id: "q_demo0005",
      org: "acme",
      repo: "acme/infra",
      issue: 1878,
      title: "Rotate the staging certificate",
      branch: "colonizer/issue-1878-q_demo0005",
      host: "gpu-lab",
      agent: "codex-cli",
      status: "blocked",
      created_at: ago(31),
      priority: 5,
      reason: "blocked",
      detail: "waiting on the colony it is stacked on, which is parked",
      attention: null,
      resumes_at: null,
      held: false,
      policy_hold: false,
      actions: { resume: false, stop: true, restart: false },
      why_not: { resume: "only parked colonies can be resumed", restart: "already on the current version" },
    },
    {
      id: "q_demo0006",
      org: "acme",
      repo: "acme/app",
      issue: 1203,
      title: "Offline queue for tap-to-pay",
      branch: "colonizer/issue-1203-q_demo0006",
      host: "gpu-lab",
      agent: "claude-code",
      status: "queued",
      created_at: ago(75),
      priority: 0,
      reason: null,
      detail: null,
      attention: { reason: "stalled", since: ago(40), nudges: 2 },
      resumes_at: null,
      held: false,
      policy_hold: false,
      actions: { resume: false, stop: true, restart: false },
      why_not: { resume: "only parked colonies can be resumed", restart: "already on the current version" },
    },
    {
      id: "q_demo0007",
      org: "acme",
      repo: "acme/webshop",
      issue: 77,
      title: "Split the checkout bundle",
      branch: "colonizer/issue-77-q_demo0007",
      host: "build-box",
      agent: "claude-code",
      status: "queued",
      created_at: ago(18),
      priority: 0,
      reason: null,
      detail: "the org's merge freeze holds new colonies until Friday",
      attention: null,
      resumes_at: null,
      held: false,
      policy_hold: true,
      actions: { resume: false, stop: true, restart: false },
      why_not: {
        resume: "only parked colonies can be resumed",
        restart: "on a release policy hold; release it from the colony's page first",
      },
    },
    {
      id: "q_demo0008",
      org: "acme",
      repo: "acme/api",
      issue: 90,
      title: "Drop the unused index",
      branch: "colonizer/issue-90-q_demo0008",
      host: "build-box",
      agent: "claude-code",
      status: "queued",
      created_at: ago(63),
      priority: 0,
      reason: null,
      detail: null,
      attention: null,
      resumes_at: null,
      held: true,
      policy_hold: false,
      actions: { resume: false, stop: true, restart: false },
      why_not: {
        resume: "superseded by https://github.com/acme/api/pull/90: files — this colony's work was covered; Keep it first to start it anyway",
        restart: "already on the current version",
      },
    },
    {
      // Done but still flagged: the watchdog's flag keeps it on the page, and nothing is left to do.
      id: "q_demo0009",
      org: "acme",
      repo: "acme/infra",
      issue: null,
      title: "Rotate the deploy keys",
      branch: null,
      host: "gpu-lab",
      agent: "codex-cli",
      status: "stopped",
      created_at: ago(90),
      priority: 0,
      reason: "nudges_exhausted",
      detail: null,
      attention: { reason: "nudges_exhausted", since: ago(80), nudges: 3 },
      resumes_at: null,
      held: false,
      policy_hold: false,
      actions: { resume: false, stop: false, restart: false },
      why_not: {
        resume: "only parked colonies can be resumed",
        stop: "already stopped",
        restart: "already on the current version",
      },
    },
  ],
  hosts: [
    { name: "build-box", reachable: true, slots_in_use: 2, slots_ceiling: 4, running: 2, parked: 2, queued: 4, queue_depth: 6, over_ceiling: false },
    { name: "gpu-lab", reachable: true, slots_in_use: 5, slots_ceiling: 4, running: null, parked: null, queued: null, queue_depth: 2, over_ceiling: true },
  ],
  draining: false,
  external_writes_blocked: false,
  // The server counts only the plainly queued colonies (queues.rs), so not every row.
  queue_depth: 4,
};

/** What the mock's rows match each filter against, so `?mock=1` narrows like the server would. */
function matches(row: QueuesRow, f: QueuesFilters): boolean {
  if (f.host && row.host !== f.host) return false;
  if (f.reason && row.reason !== f.reason) return false;
  // The server takes the bare repo name as well as `org/repo`; so does this.
  if (f.repo && row.repo !== f.repo && repoKey(row).split("/").pop() !== f.repo) return false;
  if (f.agent && row.agent !== f.agent) return false;
  if (f.q) {
    const needle = f.q.toLowerCase();
    const hay = [row.id, row.org, row.repo, row.title, row.branch, row.reason, row.detail].filter(Boolean).join(" ").toLowerCase();
    if (!hay.includes(needle)) return false;
  }
  return true;
}

export function queuesMock(_ms: MockState): QueuesApi {
  return {
    queues: async (filters) => {
      const rows = queuesSample.rows.filter((row) => matches(row, filters ?? {}));
      return { ...clone(queuesSample), rows: clone(rows) };
    },
  };
}
