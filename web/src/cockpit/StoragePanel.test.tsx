// The storage panel (issue #220): categories with the microsandbox home row marked as holding the
// kept image cache, the reclaimable total counted from PR-opened colonies only, and the
// admission-paused notice. Plus the log-archive section (issue #496): size, and a retention form
// whose Apply is gated on a preview. Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { prReclaimable, retentionSummary, StoragePanelView } from "./StoragePanel";
import type { ArchiveListing, StorageSummary } from "../types";

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

const ARCHIVE: ArchiveListing = {
  root: "/var/lib/colonizer/archive",
  count: 2,
  bytes: 5_242_880 + 2_621_440,
  entries: [
    { session: "old98765", repo: "acme/webshop", issue: 61, title: "Checkout fails for guest users", status: "pr_opened", bundle: "old98765-rev1.tar.zst", bytes: 5_242_880, archived_at: "2026-09-19T10:00:00Z", revision: 1 },
    { session: "merge5678", repo: "acme/design-system", issue: 18, title: "Dark mode palette drift", status: "merged", bundle: "merge5678-rev2.tar.zst", bytes: 2_621_440, archived_at: "2026-09-21T12:00:00Z", revision: 2 },
  ],
};

const archiveMarkup = () =>
  renderToStaticMarkup(
    <StoragePanelView summary={SUMMARY} onOpenColony={() => {}} onCleanup={() => {}} cleaningId={null} archive={ARCHIVE} onRetention={async () => ({ dry_run: true, remove: [], count: 0, bytes: 0, kept_single_copy: 0 })} onArchiveChanged={() => {}} />,
  );

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

  it("shows the log archive's bundle count and size, with Apply gated behind a preview", () => {
    const out = archiveMarkup();
    expect(out).toContain("LOG ARCHIVE");
    expect(out).toContain("2 bundles · 7.5M");
    expect(out).toContain("Automatic cleanup");
    expect(out).toContain("Allow deleting the only copy");
    expect(out).toContain("Preview");
    // No preview yet, so the danger button cannot fire: the only disabled control is Apply.
    expect(out).toContain("disabled");
    // An older mothership without /api/archive hides the whole section.
    expect(markup()).not.toContain("LOG ARCHIVE");
  });

  it("words a preview as what it would remove, and a single-copy hold as nothing going anywhere", () => {
    const plan = (count: number, bytes: number, kept: number) => ({ dry_run: true, remove: [], count, bytes, kept_single_copy: kept });
    expect(retentionSummary(plan(0, 0, 0))).toBe("This would remove 0 bundles, 0B");
    expect(retentionSummary(plan(1, 2048, 0))).toBe("This would remove 1 bundle, 2K");
    expect(retentionSummary(plan(2, 3 * 1024 ** 2, 0))).toBe("This would remove 2 bundles, 3M");
    expect(retentionSummary(plan(0, 0, 1))).toBe(`Nothing would be removed: 1 bundle is the only copy (tick "Allow deleting the only copy")`);
    expect(retentionSummary(plan(0, 0, 2))).toBe(`Nothing would be removed: 2 bundles are the only copy (tick "Allow deleting the only copy")`);
  });
});
