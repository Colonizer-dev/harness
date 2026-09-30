// The in-browser mock's Docs & README loop (`?mock=1`): settings kept in memory, and a canned report
// so the Loops page's card has something to show.
import { ApiError, type Api } from "../api";
import { allowEntryError, type DocsLoopView, type DocsReport } from "./docsLoop";

type DocsLoopApi = Pick<Api, "docsLoop" | "saveDocsLoop" | "setDocsLoopTarget" | "runDocsLoop">;

export function mockDocsLoop(now: () => string): DocsLoopApi {
  const view: DocsLoopView = {
    name: "Docs & README",
    settings: { allow: [], interval_hours: 24, cooldown_hours: 24 },
    enabled: false,
    next_run_at: null,
    last_report: null,
    history: [],
    limits: { min_interval_hours: 1, max_interval_hours: 168, max_cooldown_hours: 720 },
  };
  const copy = (): DocsLoopView => JSON.parse(JSON.stringify(view));
  const settle = () => {
    view.enabled = view.settings.allow.length > 0;
    view.next_run_at = view.enabled ? new Date(Date.now() + 10 * 60_000).toISOString() : null;
  };
  const report = (dry: boolean): DocsReport => ({
    id: `docs_${Math.random().toString(16).slice(2, 10)}`,
    at: now(),
    trigger: dry ? "dry_run" : "run_now",
    dry_run: dry,
    external_writes_blocked: false,
    repos: view.settings.allow.map((repo) => ({
      repo: repo.includes("/") ? repo : `${repo}/webshop`,
      head: "4f1c2a9e0b7d",
      since: "9a8b7c6d5e4f",
      findings: [
        { kind: "undocumented_change", file: "docs/loops.md", change: "#128", message: "`src/loops.rs` changed in #128 without an update to docs/loops.md, which documents it; re-check docs/loops.md against the code as it is now" },
        { kind: "broken_anchor", file: "README.md", line: 42, message: "docs/install.md#linux: no heading or anchor #linux in docs/install.md" },
      ],
      more: 0,
      action: dry ? "report_only" : "dispatched",
      reason: dry ? "dry run: findings only, nothing dispatched" : "dispatched docs colony mock1234",
      colony: null,
    })),
  });
  return {
    docsLoop: async () => copy(),
    saveDocsLoop: async (settings) => {
      view.settings = { ...settings, allow: [...settings.allow] };
      settle();
      return copy();
    },
    setDocsLoopTarget: async (target, enabled) => {
      const error = allowEntryError(target);
      if (enabled && error) throw new ApiError(error, 400);
      const has = view.settings.allow.some((a) => a.toLowerCase() === target.toLowerCase());
      if (enabled && !has) view.settings.allow.push(target);
      if (!enabled) {
        if (!has) throw new ApiError(`${target} is not on the allowlist`, 404);
        view.settings.allow = view.settings.allow.filter((a) => a.toLowerCase() !== target.toLowerCase());
      }
      settle();
      return copy();
    },
    runDocsLoop: async (dry) => {
      if (!view.enabled) throw new ApiError("the Docs & README loop is off: enable it for a repository or org first", 409);
      const r = report(dry);
      if (!dry) {
        view.last_report = r;
        view.history.unshift({ id: r.id, at: r.at, trigger: r.trigger, summary: `${r.repos.length} repositories` });
      }
      return r;
    },
  };
}
