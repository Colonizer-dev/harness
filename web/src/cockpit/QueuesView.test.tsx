// The Queues page (issue #1127), rendered to static markup: this codebase keeps tests off jsdom,
// and renderToStaticMarkup runs no effects, so the page's own fetch never fires — the rows come in
// through the `initialPayload` prop and the filtered states through `initialFilters`, the same way
// OverviewView's tests exercise its filters. Static markup cannot click, so the selection and the
// bulk bar are not exercised here.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { createMockApi } from "../mock";
import { queuesSample } from "../features/queues/mock";
import type { QueuesFilters, QueuesPayload } from "../features/queues/types";
import { ApiContext } from "../context";
import { QueuesView } from "./QueuesView";

const api = createMockApi();

const render = (payload: QueuesPayload | null = queuesSample, filters: QueuesFilters | null = null) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={api}>
      <QueuesView sessions={[]} initialPayload={payload} initialFilters={filters} />
    </ApiContext.Provider>,
  );

describe("QueuesView", () => {
  it("renders waiting rows with their status badge, colony link and branch", () => {
    const html = render();
    expect(html).toContain(">Queues</h1>");
    expect(html).toContain("4 waiting");
    for (const badge of [">Queued</span>", ">Parked</span>", ">Needs your answer</span>", ">Blocked</span>"]) {
      expect(html).toContain(badge);
    }
    expect(html).toContain("acme/webshop#42");
    // The server's repo already carries the org: the name must not double it.
    expect(html).not.toContain("acme/acme/");
    // An issue-less colony (q_demo0009) names itself without a dangling "#null".
    expect(html).not.toContain("#null");
    expect(html).toContain("Checkout fails for guest users");
    expect(html).toContain("colonizer/issue-42-q_demo0001");
    expect(html).toContain('href="/colonies/q_demo0001"');
  });

  it("names wait reasons in the maps' words, with the resume time appended", () => {
    const html = render();
    expect(html).toContain("provider quota exhausted · resumes");
    expect(html).toContain("repo&#x27;s daily PR cap reached");
    // An attention row reads attentionText, a plain row its status.
    expect(html).toContain("No progress, nudged 2×");
    expect(html).toContain("Waiting for your answer");
  });

  it("highlights a host over its ceiling and shows the reachable ones plainly", () => {
    const html = render();
    expect(html).toContain(">over ceiling</span>");
    expect(html).toContain("5/4");
    expect(html).toContain("4 queued · 2 parked");
    // A fleet peer this mothership cannot count reads "—" rather than zeros.
    expect(html).toContain("— · —");
  });

  it("refuses a policy-hold row visibly: the hold badge, the detail, and no action offered", () => {
    const html = render();
    expect(html).toContain(">release policy hold</span>");
    expect(html).toContain("the org&#x27;s merge freeze holds new colonies until Friday");
    expect(html).toContain("no action");
    // The refused row's own why is on the disabled buttons elsewhere; the hold row offers nothing clickable.
    expect(html).toContain(">Resume</button>");
    expect(html).toContain("disabled");
  });

  it("badges a held row as superseded", () => {
    expect(render()).toContain("held · superseded");
  });

  it("shows a row's raised priority", () => {
    expect(render()).toContain(">P5</span>");
  });

  it("renders group headers when grouping by reason", () => {
    const html = render(queuesSample, { group: "reason" });
    expect(html).toContain(">Queued <span");
    expect(html).toContain(">provider quota exhausted <span");
    expect(html).toContain(">repo&#x27;s daily PR cap reached <span");
  });

  it("says when a drain holds the queue and when external writes are blocked", () => {
    expect(render({ ...queuesSample, draining: true })).toContain(
      "Draining for an update or restart: new colonies stay queued until it finishes.",
    );
    expect(render({ ...queuesSample, external_writes_blocked: true })).toContain("publishes are refused");
    expect(render()).not.toContain("Draining for an update");
  });

  it("reads a filtered initial state without the address", () => {
    const html = render(queuesSample, { host: "gpu-lab", q: "certificate" });
    expect(html).toContain("value=\"certificate\"");
  });

  it("loads, and keeps an empty queue honest", () => {
    expect(render(null)).toContain("Loading the queues…");
    expect(render({ ...queuesSample, rows: [] })).toContain("nothing is queued");
  });
});

describe("the queues mock", () => {
  it("answers the payload and narrows by the filters like the server would", async () => {
    const full = await api.queues();
    expect(full.rows).toHaveLength(queuesSample.rows.length);
    expect(full.queue_depth).toBe(4);
    expect(full.hosts.map((h) => h.name)).toEqual(["build-box", "gpu-lab"]);
    const narrowed = await api.queues({ host: "build-box", q: "idempotency" });
    expect(narrowed.rows).toHaveLength(1);
    expect(narrowed.rows[0].id).toBe("q_demo0002");
  });
});
