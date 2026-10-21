// @vitest-environment happy-dom
// Issue #1389: Popover must render inside modal dialogs so it appears above the backdrop
import { createRoot } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Popover, SearchList } from "./chat/Popover";
import { RepoMultiSelect } from "../components/RepoMultiSelect";
import { ApiContext } from "../context";
import type { Api } from "../api";
import type { Repo, OrgInfo } from "../types";

(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

if (!HTMLDialogElement.prototype.showModal) {
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
}
if (!HTMLDialogElement.prototype.close) {
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
}

const mockRepos: Repo[] = [{ full_name: "acme/widgets", description: "Main product", private: false, fork: false, archived: false, open_issues_count: 0, pushed_at: "2026-01-01" }];
const mockOrgs: OrgInfo[] = [{ org: "acme", name: "acme", avatar_url: "https://avatars.example.com/acme", visible: true, member: true, public_repos: 1, hidden: false } as any];

describe("Popover in modal dialogs (issue #1389)", () => {
  let container: HTMLDivElement;
  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
  });
  afterEach(() => {
    document.body.removeChild(container);
  });

  it("Popover falls back to document.body when no dialog is open", async () => {
    const anchor = document.createElement("button");
    container.appendChild(anchor);
    const root = createRoot(container);
    root.render(
      <Popover open={true} onClose={() => {}} anchor={{ current: anchor }} label="test">
        <SearchList items={[{ id: "x", label: "X" }]} onPick={() => {}} selected={null} />
      </Popover>,
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(document.querySelector('[role="dialog"][aria-label="test"]')).toBeTruthy();
    root.unmount();
  });

  async function testLoopCardPicker(spec: { CardComponent: React.ComponentType<any>; cardProps?: Record<string, any>; saveFnName: keyof Api }) {
    const dialog = document.createElement("div");
    container.appendChild(dialog);
    const mockApi = {
      repos: vi.fn(async () => mockRepos),
      orgs: vi.fn(async () => mockOrgs),
      docsLoop: vi.fn(async () => ({ id: "docs", name: "Docs & README", enabled: false, next_run_at: null, last_report: null, settings: { allow: [], interval_hours: 24, cooldown_hours: 24 } })),
      saveDocsLoop: vi.fn(async (s) => ({ id: "docs", name: "Docs & README", enabled: false, next_run_at: null, last_report: null, settings: s })),
      mergeLoop: vi.fn(async () => ({ id: "merge", name: "Merge train", enabled: false, running: false, next_run_at: null, last_report: null, writes_blocked: false, repos: {}, settings: { enabled: false, allow: [], never: [], local_checks: [], cadence: { every: "interval", minutes: 60 }, max_merges: 5, cooldown_secs: 120, ci_wait_minutes: 10, fix_attempts: 2, flaky_checks: [], self_heal: false, revert_on_red: false, redo_on_conflict: false, resolve_conflicts: false, fix_red: false, repo_max_merges: {} } })),
      saveMergeLoop: vi.fn(async (s) => ({ id: "merge", name: "Merge train", enabled: false, running: false, next_run_at: null, last_report: null, writes_blocked: false, repos: {}, settings: s })),
      supplyChainLoop: vi.fn(async () => ({ id: "supply", name: "Dependencies & supply chain", enabled: false, running: false, next_run_at: null, last_report: null, blocked: false, attention: [], scanners: {}, settings: { enabled: false, allow: [], cadence: { every: "interval", minutes: 60 }, min_severity: "high", max_per_repo: 2, max_per_run: 5, cooldown_hours: 24, outdated: false, builtin: true } })),
      saveSupplyChainLoop: vi.fn(async (s) => ({ id: "supply", name: "Dependencies & supply chain", enabled: false, running: false, next_run_at: null, last_report: null, blocked: false, attention: [], scanners: {}, settings: s })),
      loopHistory: vi.fn(async () => {
        const today = new Date();
        const days = Array.from({ length: 7 }, (_, i) => {
          const d = new Date(today);
          d.setDate(d.getDate() - (6 - i));
          const y = d.getFullYear(), m = String(d.getMonth() + 1).padStart(2, "0"), da = String(d.getDate()).padStart(2, "0");
          return `${y}-${m}-${da}`;
        });
        return { days: 7, from: days[0], to: days[6], totals: { runs: 0, failed: 0, cost_usd: 0 }, buckets: days.map((day) => ({ day, runs: 0, ok: 0, partial: 0, failed: 0, skipped: 0, running: 0, cost_usd: 0 })), runs: [], last: null };
      }),
    } as any as Api;
    const root = createRoot(dialog);
    root.render(<ApiContext.Provider value={mockApi}><spec.CardComponent {...spec.cardProps} onOpenColony={() => {}} /></ApiContext.Provider>);
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 200)); });
    const cardButton = dialog.querySelector("button[type='button']") as HTMLElement;
    expect(cardButton).toBeTruthy();
    act(() => { cardButton?.click?.(); });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 100)); });
    const detailDialog = document.querySelector('dialog[aria-labelledby]') as HTMLDialogElement;
    expect(detailDialog).toBeTruthy();
    const pickerButton = detailDialog?.querySelectorAll('[aria-haspopup="listbox"]')?.[0] as HTMLElement;
    expect(pickerButton).toBeTruthy();
    act(() => { pickerButton?.click?.(); });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 100)); });
    const popover = document.querySelector('[role="dialog"][aria-label*="Choose"]') as HTMLElement;
    expect(popover).toBeTruthy();
    expect(detailDialog?.contains(popover)).toBe(true);
    expect(popover?.textContent).toContain("widgets");
    const repoOption = Array.from(popover?.querySelectorAll('[role="option"]') ?? []).find((el) => el.textContent?.includes("widgets")) as HTMLElement;
    expect(repoOption).toBeTruthy();
    act(() => { repoOption?.click?.(); });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 100)); });
    const saveButton = Array.from(detailDialog?.querySelectorAll('button') ?? []).find((el) => el.textContent?.includes("Save")) as HTMLElement;
    if (saveButton) {
      act(() => { saveButton?.click?.(); });
      await act(async () => { await new Promise((resolve) => setTimeout(resolve, 100)); });
    }
    expect((mockApi as any)[spec.saveFnName]).toHaveBeenCalled();
    const saveCall = ((mockApi as any)[spec.saveFnName] as any).mock.calls[0];
    expect(saveCall[0]?.allow).toContain("acme/widgets");
    root.unmount();
  }

  it("DocsLoopCard picker works inside detail dialog", async () => {
    const { DocsLoopCard } = await import("./DocsLoopCard");
    await testLoopCardPicker({
      CardComponent: DocsLoopCard,
      saveFnName: "saveDocsLoop",
    });
  });

  it("MergeLoopCard picker works inside detail dialog", async () => {
    const { MergeLoopCard } = await import("./MergeLoopCard");
    await testLoopCardPicker({
      CardComponent: MergeLoopCard,
      cardProps: { repos: mockRepos },
      saveFnName: "saveMergeLoop",
    });
  });

  it("SupplyChainLoopCard picker works inside detail dialog", async () => {
    const { SupplyChainLoopCard } = await import("./SupplyChainLoop");
    await testLoopCardPicker({
      CardComponent: SupplyChainLoopCard,
      saveFnName: "saveSupplyChainLoop",
    });
  });

  it("RepoMultiSelect shows error when fetch fails", async () => {
    const dialog = document.createElement("dialog");
    dialog.setAttribute("open", "");
    container.appendChild(dialog);

    const api = { repos: () => Promise.reject(new Error("Network error")), orgs: () => Promise.resolve([]) } as unknown as Api;
    const root = createRoot(dialog);
    root.render(
      <ApiContext.Provider value={api}>
        <RepoMultiSelect value={[]} onChange={() => {}} label="test" />
      </ApiContext.Provider>,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    const button = dialog.querySelector('[aria-haspopup="listbox"]') as HTMLElement;
    act(() => {
      button?.click?.();
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    const popover = document.querySelector('[role="dialog"]');
    expect(popover?.textContent).toContain("Network error");
    expect(dialog.contains(popover)).toBe(true);
    root.unmount();
  });

  it("RepoMultiSelect retries fetch after error on next open", async () => {
    const dialog = document.createElement("dialog");
    dialog.setAttribute("open", "");
    container.appendChild(dialog);

    let shouldFail = true;
    const api = {
      repos: () => (shouldFail ? Promise.reject(new Error("Fetch failed")) : Promise.resolve(mockRepos)),
      orgs: () => Promise.resolve(mockOrgs),
    } as unknown as Api;

    const root = createRoot(dialog);
    root.render(
      <ApiContext.Provider value={api}>
        <RepoMultiSelect value={[]} onChange={() => {}} label="test" />
      </ApiContext.Provider>,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    const button = dialog.querySelector('[aria-haspopup="listbox"]') as HTMLElement;
    act(() => {
      button?.click?.();
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    let popover = document.querySelector('[role="dialog"]');
    expect(popover?.textContent).toContain("Fetch failed");

    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });

    shouldFail = false;
    act(() => {
      button?.click?.();
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    popover = document.querySelector('[role="dialog"]');
    expect(popover?.textContent).not.toContain("Fetch failed");
    root.unmount();
  });

  it("RepoMultiSelect shows connect GitHub message when no API and no props", async () => {
    const dialog = document.createElement("dialog");
    dialog.setAttribute("open", "");
    container.appendChild(dialog);

    const root = createRoot(dialog);
    root.render(
      <ApiContext.Provider value={null}>
        <RepoMultiSelect value={[]} onChange={() => {}} label="test" allowAll={false} />
      </ApiContext.Provider>,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    const button = dialog.querySelector('[aria-haspopup="listbox"]') as HTMLElement;
    expect(button).toBeTruthy();
    act(() => {
      button?.click?.();
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    const popover = document.querySelector('[role="dialog"]');
    expect(popover?.textContent).toContain("No repositories or orgs found: connect GitHub in Settings.");
    root.unmount();
  });

  it("RepoMultiSelect shows connect GitHub message when API exists but both repos and orgs are empty", async () => {
    const dialog = document.createElement("dialog");
    dialog.setAttribute("open", "");
    container.appendChild(dialog);

    const api = { repos: () => Promise.resolve([]), orgs: () => Promise.resolve([]) } as unknown as Api;
    const root = createRoot(dialog);
    root.render(
      <ApiContext.Provider value={api}>
        <RepoMultiSelect value={[]} onChange={() => {}} label="test" />
      </ApiContext.Provider>,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    const button = dialog.querySelector('[aria-haspopup="listbox"]') as HTMLElement;
    expect(button).toBeTruthy();
    act(() => {
      button?.click?.();
    });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });

    const popover = document.querySelector('[role="dialog"]');
    expect(popover?.textContent).toContain("No repositories or orgs found: connect GitHub in Settings.");
    root.unmount();
  });
});
