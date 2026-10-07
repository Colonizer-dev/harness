// The `loops` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { clone, isLive, now, sleep } from "../../mockShared";
import { mockDocsLoop } from "../../cockpit/docsLoopMock";
import type { MergeLoopReport, RedTeamRun, StartRedTeamRunRequest, SupplyChainReport, TsAnyReport } from "../../types";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { LoopsApi } from "./api";

export function loopsMock(ms: MockState): LoopsApi {
  return {
    redTeamRuns: () =>
      ms.later(() => [...ms.redRuns].sort((a, b) => b.created_at.localeCompare(a.created_at))),
    startRedTeamRun: async (body: StartRedTeamRunRequest) => {
      await sleep(400);
      const repo = body.repo.trim();
      if (!repo) throw new ApiError("pick a repository to raid", 400);
      const swarm = body.swarm_size ?? 3;
      if (!Number.isInteger(swarm) || swarm < 1 || swarm > 8) throw new ApiError("swarm size must be between 1 and 8", 400);
      const live = [...ms.sessions.values()].filter((s) => isLive(s.session.status)).length;
      // The server's refusals, in its order (issue #212): the nest gate applies to a start-now
      // create only, while another run still active on the repo rejects the create however it arms.
      if (!body.arm && live > 0) {
    throw new ApiError(
      `the nest is busy: ${live} colony${live === 1 ? "" : "ies"} live`,
      409,
    );
      }
      if (ms.redActive(repo)) {
        const holder = ms.redRuns.find((r) => r.repo === repo && r.state !== "done" && r.state !== "stopped" && r.state !== "cancelled");
        throw new ApiError(`a red-team run (${holder?.id ?? "?"}) is already active for ${repo}`, 409);
      }
      const run: RedTeamRun = {
    id: `rt-${Math.random().toString(16).slice(2, 8)}`,
    repo,
    org: repo.split("/")[0] ?? repo,
    state: body.arm ? "armed" : "running",
    swarm_size: swarm,
    modules: [...(body.modules ?? [])],
    autofix: body.autofix ?? false,
    // Hunters are the repo's own sessions wearing red: a fresh raid rides the work the
    // nest already cleared, and each hunter keeps its colony, so the card can join them.
    hunters: [...ms.sessions.values()]
      .filter((s) => s.session.repo === repo)
      .sort((a, b) => b.session.updated_at.localeCompare(a.session.updated_at))
      .slice(0, swarm)
      .map((s) => ({
        session_id: s.session.id,
        title: s.session.issue_title || repo,
        module: (body.modules ?? [])[0] ?? "harness",
        version: null,
        focus: "adversarial pass",
      })),
    counts: { found: 0, validated: 0, rejected: 0, filed: 0, merged: null },
    synthesis: null,
    created_at: now(),
    started_at: body.arm ? null : now(),
    ended_at: null,
    gate_reason:
      body.arm && live > 0 ? `${live} colony${live === 1 ? "" : "ies"} live — the nest must empty first` : null,
    hunter: body.hunter ?? "swarm",
    model: body.model ?? null,
    subagent_model: body.subagent_model ?? null,
    schedule_id: null,
    preset: body.preset ?? "general",
    prescan: null,
      };
      ms.redRuns.unshift(run);
      return clone(run);
    },
    loops: () => ms.later(() => ms.loopList.map(clone)),
    mergeTrain: () => ms.later(() => ({ repos: [] })),
    supplyChainLoop: () => ms.later(() => clone(ms.supplyLoop)),
    saveSupplyChainLoop: async (settings) => {
      await sleep(150);
      if (settings.cadence.every === "interval" && settings.cadence.minutes < 60) throw new ApiError("the supply-chain loop runs at most hourly", 400);
      const active = settings.enabled && settings.allow.length > 0;
      ms.supplyLoop = { ...ms.supplyLoop, settings: clone(settings), next_run_at: active ? new Date(Date.now() + 6 * 3_600_000).toISOString() : null };
      return clone(ms.supplyLoop);
    },
    runSupplyChainLoop: async (body) => {
      await sleep(400);
      const report: SupplyChainReport = { ...ms.supplySample, id: `scr_${Math.random().toString(16).slice(2, 8)}`, dry_run: body.dry_run, trigger: "manual", started_at: now(), finished_at: now() };
      // The mock never starts colonies: a real run says what it would have started, like a dry run.
      report.dispatched = report.dispatched.map((d) => ({ ...d, session: null }));
      if (!body.dry_run) ms.supplyLoop = { ...ms.supplyLoop, last_report: report, attention: report.attention };
      return clone(report);
      },
    mergeLoop: () => ms.later(() => clone(ms.mergeLoop)),
    saveMergeLoop: async (settings) => {
      await sleep(150);
      ms.mergeLoop = { ...ms.mergeLoop, settings: clone(settings), next_run_at: settings.enabled ? new Date(Date.now() + 60 * 60_000).toISOString() : null };
      return clone(ms.mergeLoop);
    },
    runMergeLoop: async (dryRun) => {
      await sleep(300);
      const at = now();
      const report: MergeLoopReport = { started_at: at, finished_at: at, dry_run: true, forced_dry_run: !dryRun, stopped: null, api_calls: 0, summary: "dry run: would merge 0 · would update 0 · red 0 · would dispatch redo 0 · skipped 0", lines: [], repos: [] };
      ms.mergeLoop = { ...ms.mergeLoop, last_report: report, history: [report, ...ms.mergeLoop.history] };
      return { started: false, report: clone(report) };
    },
    createLoop: async (body) => {
      await sleep(200);
      const l = ms.loopOf(body, `loop_${Math.random().toString(16).slice(2, 8)}`, now());
      ms.loopList.push(l);
      return clone(l);
    },
    updateLoop: async (id, body) => {
      await sleep(150);
      const at = ms.loopList.findIndex((l) => l.id === id);
      if (at < 0) throw new ApiError("no such loop", 404);
      if (ms.loopList[at].kind === "disk_cleanup") {
        const was = ms.loopList[at];
        const enabled = body.enabled ?? was.enabled;
        ms.loopList[at] = {
          ...was,
          cadence: body.cadence,
          enabled,
          next_run_at: enabled ? new Date(Date.now() + (body.cadence.every === "interval" ? body.cadence.minutes : 60) * 60_000).toISOString() : null,
          disk_cleanup: { ...was.disk_cleanup!, settings: body.disk_cleanup ?? was.disk_cleanup!.settings },
        };
        return clone(ms.loopList[at]);
      }
      ms.loopList[at] = { ...ms.loopOf(body, id, ms.loopList[at].created_at, ms.loopList[at].runs), last_run: ms.loopList[at].last_run };
      return clone(ms.loopList[at]);
    },
    deleteLoop: async (id) => {
      await sleep(120);
      const at = ms.loopList.findIndex((l) => l.id === id);
      if (at >= 0) ms.loopList.splice(at, 1);
    },
    runLoopNow: async (id) => {
      await sleep(200);
      const l = ms.loopList.find((x) => x.id === id);
      if (!l) throw new ApiError("no such loop", 404);
      throw new ApiError("the mock mothership does not launch colonies from loops", 409);
    },
    runDiskCleanup: async (id, dryRun) => {
      await sleep(300);
      const l = ms.loopList.find((x) => x.id === id);
      if (!l?.disk_cleanup) throw new ApiError("no such loop", 404);
      const report = ms.cleanupReport(dryRun);
      if (dryRun) l.disk_cleanup.previewed_at = report.at;
      else {
        l.runs += 1;
        l.disk_cleanup.history.unshift(report);
      }
      return clone(report);
    },
    tsAnyLoop: () => ms.later(() => clone(ms.tsAnyLoop)),
    saveTsAnyLoop: async (settings) => {
      await sleep(150);
      if (settings.cadence.every === "interval" && settings.cadence.minutes < 60) throw new ApiError("the TypeScript any loop runs at most hourly", 400);
      const active = settings.enabled && settings.allow.length > 0;
      ms.tsAnyLoop = { ...ms.tsAnyLoop, settings: clone(settings), next_run_at: active ? new Date(Date.now() + 6 * 3_600_000).toISOString() : null };
      return clone(ms.tsAnyLoop);
    },
    runTsAnyLoop: async (body) => {
      await sleep(400);
      const report: TsAnyReport = { ...ms.tsAnySample, id: `tsa_${Math.random().toString(16).slice(2, 8)}`, dry_run: body.dry_run, trigger: "manual", started_at: now(), finished_at: now() };
      // The mock never starts colonies: a real run says what it would have started, like a dry run.
      report.dispatched = report.dispatched.map((d) => ({ ...d, session: null }));
      if (!body.dry_run) ms.tsAnyLoop = { ...ms.tsAnyLoop, last_report: report };
      return clone(report);
    },
    loopRuns: (id) => ms.later(() => [...ms.sessions.values()].map((s) => s.session).filter((s) => s.origin === `loop:${id}`).map(clone)),
    ...mockDocsLoop(now),
    redTeamSchedules: () => ms.later(() => ms.redSchedules.map(clone)),
    createRedTeamSchedule: async (body) => {
      await sleep(250);
      const schedule = ms.scheduleOf(body, `rts-${Math.random().toString(16).slice(2, 8)}`, now());
      ms.redSchedules.push(schedule);
      return clone(schedule);
    },
    updateRedTeamSchedule: async (id, body) => {
      await sleep(200);
      const at = ms.redSchedules.findIndex((s) => s.id === id);
      if (at < 0) throw new ApiError("no such red-team schedule", 404);
      const next = { ...ms.scheduleOf(body, id, ms.redSchedules[at].created_at), last_run_at: ms.redSchedules[at].last_run_at, last_result: ms.redSchedules[at].last_result };
      ms.redSchedules[at] = next;
      return clone(next);
    },
    deleteRedTeamSchedule: async (id) => {
      await sleep(150);
      const at = ms.redSchedules.findIndex((s) => s.id === id);
      if (at < 0) throw new ApiError("no such red-team schedule", 404);
      ms.redSchedules.splice(at, 1);
    },
    probeHunter: async (id) => {
      await sleep(200);
      const strix = id === "strix";
      return {
        manifest: {
          id,
          name: strix ? "Strix" : "Shannon",
          description: strix
            ? "Open-source AI penetration-testing agents that run code dynamically and validate findings with working PoCs."
            : "Keygraph's AI pentester for web apps and APIs — no exploit, no report.",
          homepage: strix ? "https://github.com/usestrix/strix" : "https://github.com/KeygraphHQ/shannon",
          licence: strix ? "Apache-2.0" : "AGPL-3.0",
          available: strix,
          needs_docker: true,
        },
        installed: null,
        probe: { runtime_ok: false, docker_ok: false, ready: false, detail: strix ? "binary not installed yet" : "manifest-only stub" },
      };
    },
    stopRedTeamRun: async (id) => {
      await sleep(250);
      const run = ms.redRuns.find((r) => r.id === id);
      if (!run) throw new ApiError("no such red-team run", 404);
      if (run.state === "done" || run.state === "stopped" || run.state === "cancelled") throw new ApiError("this run is already over", 409);
      run.state = "stopped";
      run.ended_at = now();
      // Send the hunters home too, when there is a chapel to close: a live mock colony just
      // halts and reports stopped, the same way stopSession would.
      for (const hunter of run.hunters) {
    const s = ms.sessions.get(hunter.session_id);
    if (s && isLive(s.session.status)) {
      s.halt();
      s.patch({ status: "stopped", mesh: null });
    }
      }
      return clone(run);
    },
    cancelRedTeamRun: async (id) => {
      await sleep(250);
      const run = ms.redRuns.find((r) => r.id === id);
      if (!run) throw new ApiError("no such red-team run", 404);
      if (run.state === "done" || run.state === "stopped" || run.state === "cancelled") return clone(run);
      run.state = "cancelled";
      run.ended_at = now();
      run.cancelled_at = run.ended_at;
      run.cancelled_by = "you";
      for (const hunter of run.hunters) {
        const s = ms.sessions.get(hunter.session_id);
        if (s && isLive(s.session.status)) {
          s.halt();
          s.patch({ status: "stopped", mesh: null });
        }
      }
      return clone(run);
    },
    synthesizeRedTeamRun: async (id) => {
      await sleep(250);
      const run = ms.redRuns.find((r) => r.id === id);
      if (!run) throw new ApiError("no such red-team run", 404);
      if (run.state !== "done") throw new ApiError("synthesis starts once the raid is done", 409);
      const synth = run.synthesis;
      // Idempotent while a synthesis is already on its way, like the server.
      if (synth && (synth.state === "pending" || synth.state === "running")) return clone(run);
      // Supersede the old colony and queue a fresh one; the old report stays until the new one lands.
      run.synthesis = {
        state: "pending",
        session_id: `synth-${Math.random().toString(16).slice(2, 6)}`,
        report: synth?.report ?? null,
        reason: null,
        superseded: synth?.session_id ? [...(synth.superseded ?? []), synth.session_id] : (synth?.superseded ?? []),
      };
      return clone(run);
    }
  };
}
