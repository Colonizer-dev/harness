import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Api } from "../api";
import { ApiContext } from "../context";
import { DocsLoopPanel, DocsReportView } from "./DocsLoopCard";
import { allowEntryError, describeInterval, findingPlace, summarizeReport, type DocsLoopView, type DocsReport } from "./docsLoop";
import { mockDocsLoop } from "./docsLoopMock";

const NOW = Date.parse("2026-09-30T12:00:00Z");

const off: DocsLoopView = {
  name: "Docs & README",
  settings: { allow: [], interval_hours: 24, cooldown_hours: 24 },
  enabled: false,
  next_run_at: null,
  last_report: null,
  history: [],
  limits: { min_interval_hours: 1, max_interval_hours: 168, max_cooldown_hours: 720 },
};

const report: DocsReport = {
  id: "docs_1",
  at: "2026-09-30T10:00:00Z",
  trigger: "schedule",
  dry_run: false,
  external_writes_blocked: false,
  repos: [
    {
      repo: "acme/app",
      head: "abc",
      since: "def",
      findings: [
        { kind: "broken_link", file: "README.md", line: 3, message: "docs/x.md: no such file docs/x.md" },
        { kind: "undocumented_change", file: "docs/api.md", change: "#12", message: "`src/api.rs` changed in #12 without an update to docs/api.md" },
      ],
      more: 1,
      action: "dispatched",
      reason: "dispatched docs colony c1",
      colony: "c1",
    },
    { repo: "acme/web", head: "abc", since: null, findings: [{ kind: "missing_command", message: "`npm run gone` …" }], more: 0, action: "skipped", reason: "docs colony c0 is still open for this repository", colony: null },
    { repo: "acme/api", head: "abc", since: null, findings: [{ kind: "changelog", file: "changelog.d", message: "1 merged change touched code…", advisory: true }], more: 0, action: "clean", reason: "only advisory findings: nothing to dispatch", colony: null },
  ],
};

const noop = () => {};
const panel = (view: DocsLoopView, dryRun: DocsReport | null = null) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={{} as Api}>
      <DocsLoopPanel view={view} dryRun={dryRun} busy={false} now={NOW} open onTarget={noop} onSave={noop} onRun={noop} onOpenColony={noop} />
    </ApiContext.Provider>,
  );

describe("Docs & README loop settings", () => {
  it("reads as off, with nothing to run, until a repository or org is added", () => {
    const html = panel(off);
    expect(html).toContain("Not set up: add a repository or an org");
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Run now<\/button>/);
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Dry run<\/button>/);
    expect(html).toContain("Enable");
  });

  it("lists the allowlist with a way to remove each entry, and the interval", () => {
    const on: DocsLoopView = { ...off, enabled: true, next_run_at: "2026-09-30T13:00:00Z", settings: { allow: ["acme/app", "umbrella"], interval_hours: 1, cooldown_hours: 24 } };
    const html = panel(on);
    expect(html).toContain(">On<");
    expect(html).toContain("Hourly · next in 60m");
    expect(html).toContain('aria-label="stop running on acme/app"');
    expect(html).toContain('aria-label="stop running on umbrella"');
    expect(html).toContain("No run yet.");
    expect(html).not.toMatch(/disabled=""[^>]*>Run now/);
  });

  it("checks an entry the way the server does", () => {
    expect(allowEntryError("acme")).toBeNull();
    expect(allowEntryError("acme/app")).toBeNull();
    expect(allowEntryError("")).toContain("Name a repository");
    expect(allowEntryError("acme/app/x")).toContain("is not an owner or owner/name");
    expect(allowEntryError("not a repo")).toContain("is not an owner or owner/name");
    expect(describeInterval(24)).toBe("daily");
    expect(describeInterval(1)).toBe("hourly");
    expect(describeInterval(48)).toBe("every 2 days");
    expect(describeInterval(3)).toBe("every 3 hours");
  });

  it("enables and disables through the mock like the server", async () => {
    const api = mockDocsLoop(() => "2026-09-30T12:00:00Z");
    // The mock starts as a busy install: three repositories, one report.
    const seeded = await api.docsLoop();
    expect(seeded.enabled).toBe(true);
    for (const target of seeded.settings.allow) await api.setDocsLoopTarget(target, false);
    expect((await api.docsLoop()).enabled).toBe(false);
    await expect(api.runDocsLoop(true)).rejects.toThrow("is off");
    const on = await api.setDocsLoopTarget("acme/app", true);
    expect(on.enabled).toBe(true);
    expect(on.next_run_at).not.toBeNull();
    const dry = await api.runDocsLoop(true);
    expect(dry.dry_run).toBe(true);
    expect((await api.docsLoop()).last_report?.id).toBe(seeded.last_report?.id);
    const back = await api.setDocsLoopTarget("acme/app", false);
    expect(back.enabled).toBe(false);
    await expect(api.setDocsLoopTarget("acme/app", false)).rejects.toThrow("not on the allowlist");
  });
});

describe("Docs & README loop report", () => {
  it("shows every repository's action, reason and findings", () => {
    const html = renderToStaticMarkup(<DocsReportView report={report} now={NOW} onOpenColony={noop} />);
    expect(html).toContain("Last run");
    expect(html).toContain("2h ago");
    expect(html).toContain("Colony dispatched");
    expect(html).toContain("docs colony c0 is still open for this repository");
    expect(html).toContain("Clean");
    expect(html).toContain("broken link");
    expect(html).toContain("README.md:3");
    expect(html).toContain("code changed, docs did not");
    expect(html).toContain("…and 1 more");
    expect(html).toContain("Open colony");
    expect(html).toContain("(advisory)");
  });

  it("summarizes a run, and marks a dry run as such", () => {
    expect(summarizeReport(report)).toBe("3 repositories · 5 findings · 1 colony dispatched · 1 skipped");
    expect(summarizeReport({ ...report, dry_run: true })).toContain("dry run");
    expect(summarizeReport({ ...report, external_writes_blocked: true })).toContain("external writes blocked");
    expect(findingPlace({ kind: "changelog", message: "" })).toBe("");
    const onWithRun: DocsLoopView = { ...off, enabled: true, settings: { ...off.settings, allow: ["acme/app"] }, last_report: report };
    const html = panel(onWithRun, { ...report, dry_run: true, trigger: "dry_run" });
    expect(html).toContain("Dry run</span>");
  });
});
