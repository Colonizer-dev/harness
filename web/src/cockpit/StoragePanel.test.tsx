// The storage panel (issue #220): categories with the microsandbox home row marked as holding the
// kept image cache, the reclaimable total counted from PR-opened colonies only, and the
// admission-paused notice. Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { prReclaimable, StoragePanelView } from "./StoragePanel";
import type { StorageSummary } from "../types";

const SUMMARY: StorageSummary = {
  enabled: true,
  retention_secs: 43200,
  min_free_bytes: 1_073_741_824,
  warn_free_bytes: 5_368_709_120,
  free_bytes: 12_884_901_888,
  admission_paused: false,
  totals: {
    worktrees_bytes: 3_221_225_472,
    repos_bytes: 1_073_741_824,
    sessions_bytes: 268_435_456,
    microsandbox_bytes: 2_147_483_648,
  },
  reclaimable: [
    { id: "old98765", status: "pr_opened", pr_url: "https://github.com/acme/webshop/pull/61", bytes: 214_748_364, updated_at: "2026-09-20T10:00:00Z", due: true },
    { id: "merge5678", status: "merged", pr_url: "https://github.com/acme/design-system/pull/18", bytes: 96_468_992, updated_at: "2026-09-21T10:00:00Z", due: false },
    { id: "noch1234", status: "no_changes", pr_url: null, bytes: 41_943_040, updated_at: "2026-09-21T11:00:00Z", due: true },
  ],
  unpushed: [],
  orphans: [],
};

const markup = (summary: StorageSummary = SUMMARY) =>
  renderToStaticMarkup(<StoragePanelView summary={summary} onOpenColony={() => {}} onCleanup={() => {}} cleaningId={null} />);
describe("StoragePanel", () => {
  it("renders usage by category and free space against the warn and floor thresholds", () => {
    const out = markup();
    expect(out).toContain("worktrees 3G");
    expect(out).toContain("repos 1G");
    expect(out).toContain("sessions 256M");
    expect(out).toContain("microsandbox home 2G");
    expect(out).toContain("holds the shared image cache · kept");
    expect(out).toContain("12G free");
    expect(out).toContain("warn below 5G");
    expect(out).toContain("floor 1G");
  });

  it("totals the reclaimable bytes from PR-opened colonies, naming no-changes separately", () => {
    expect(prReclaimable(SUMMARY)).toEqual({ bytes: 214_748_364 + 96_468_992, colonies: 2 });
    const out = markup();
    expect(out).toContain("296.8M reclaimable from 2 colonies that opened PRs");
    expect(out).toContain("40M from 1 with no changes");
  });

  it("lists one row per reclaimable colony with a Clean up button each", () => {
    const out = markup();
    for (const id of ["old98765", "merge5678", "noch1234"]) expect(out).toContain(id);
    expect(out).toContain("Clean up");
    expect(out).toContain("past retention");
    expect(out).toContain("https://github.com/acme/webshop/pull/61");
  });

  it("shows the admission-paused notice only while the queue is paused", () => {
    expect(markup()).not.toContain("Queue paused");
    const out = markup({ ...SUMMARY, admission_paused: true });
    expect(out).toContain("Queue paused — low disk");
    expect(out).toContain("Unpushed work is never deleted");
    expect(out).toContain('role="status"');
  });

  it("reads no disk reading yet when the mothership has none", () => {
    expect(markup({ ...SUMMARY, free_bytes: null })).toContain("no disk reading yet");
  });

  it("shows the storage-settings gear only when an opener is given", () => {
    expect(markup()).not.toContain("Storage settings");
    const out = renderToStaticMarkup(
      <StoragePanelView summary={SUMMARY} onOpenColony={() => {}} onCleanup={() => {}} cleaningId={null} onOpenSettings={() => {}} />,
    );
    expect(out).toContain('aria-label="Storage settings"');
  });
});
