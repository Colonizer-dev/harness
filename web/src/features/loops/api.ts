// Loops & red-team API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request } from "../../http";
import type { DocsLoopSettings, DocsLoopView, DocsReport } from "../../cockpit/docsLoop";
import type { Session } from "../sessions/types";
import type { DiskCleanupReport, HunterProbe, Loop, MergeLoopReport, MergeLoopSettings, MergeLoopView, MergeTrainStatus, NewLoop, NewRedTeamSchedule, RedTeamRun, RedTeamSchedule, StartRedTeamRunRequest, SupplyChainLoop, SupplyChainReport, SupplyChainSettings, TsAnyLoop, TsAnyReport, TsAnySettings } from "./types";

export interface LoopsApi {
  /** Red-team runs: a swarm of hunter colonies raiding one repository (issue #212). 409 without `arm` when any colony is live or a run is already active for the repo. */
  redTeamRuns(): Promise<RedTeamRun[]>;
  startRedTeamRun(body: StartRedTeamRunRequest): Promise<RedTeamRun>;
  stopRedTeamRun(id: string): Promise<RedTeamRun>;
  /** POST /api/redteam/runs/{id}/synthesize (issue #309): (re)launch the run's synthesis colony. 409 unless the run is done; idempotent while one is pending/running. */
  synthesizeRedTeamRun(id: string): Promise<RedTeamRun>;
  /** GET /api/loops: the scheduled colonies. */
  loops(): Promise<Loop[]>;
  createLoop(body: NewLoop): Promise<Loop>;
  updateLoop(id: string, body: NewLoop): Promise<Loop>;
  deleteLoop(id: string): Promise<void>;
  /** POST /api/loops/{id}/run-now: start the next run now (409 while the previous run is live). */
  runLoopNow(id: string): Promise<Session>;
  /** GET /api/loops/{id}/runs: the loop's colonies, newest first. */
  loopRuns(id: string): Promise<Session[]>;
  /** POST /api/loops/disk-cleanup/run-now: a disk-cleanup run, or with `dryRun` a preview that removes nothing. */
  runDiskCleanup(id: string, dryRun: boolean): Promise<DiskCleanupReport>;
  /** GET /api/docs-loop: the built-in Docs & README loop — settings, next run, last report, history. */
  docsLoop(): Promise<DocsLoopView>;
  saveDocsLoop(settings: DocsLoopSettings): Promise<DocsLoopView>;
  /** POST /api/docs-loop/enable|disable: add or remove a repository or org. */
  setDocsLoopTarget(target: string, enabled: boolean): Promise<DocsLoopView>;
  /** POST /api/docs-loop/run: a run now; a dry run launches and records nothing. */
  runDocsLoop(dryRun: boolean): Promise<DocsReport>;
  /** GET /api/ts-any-loop: the built-in "TypeScript: remove any" loop. */
  tsAnyLoop(): Promise<TsAnyLoop>;
  /** PUT /api/ts-any-loop: replaces its settings (off, with an empty allowlist, by default). */
  saveTsAnyLoop(settings: TsAnySettings): Promise<TsAnyLoop>;
  /** POST /api/ts-any-loop/run: a run now, or a dry run that writes nothing. 409 while one runs. */
  runTsAnyLoop(body: { dry_run: boolean; repo?: string }): Promise<TsAnyReport>;
  /** GET /api/merge-train: the merge train per repository (issue #671); empty until a repository opts in. */
  mergeTrain(): Promise<MergeTrainStatus>;
  /** GET /api/supply-chain-loop: the built-in dependencies and supply-chain loop. */
  supplyChainLoop(): Promise<SupplyChainLoop>;
  /** PUT /api/supply-chain-loop: replaces its settings (off, with an empty allowlist, by default). */
  saveSupplyChainLoop(settings: SupplyChainSettings): Promise<SupplyChainLoop>;
  /** POST /api/supply-chain-loop/run: a run now, or a dry run that writes nothing. 409 while one runs. */
  runSupplyChainLoop(body: { dry_run: boolean; repo?: string }): Promise<SupplyChainReport>;
  /** GET /api/merge-train/loop: the merge-train loop's settings, paused repositories and run history (issue #754). */
  mergeLoop(): Promise<MergeLoopView>;
  /** PUT /api/merge-train/loop: replaces the settings. */
  saveMergeLoop(settings: MergeLoopSettings): Promise<MergeLoopView>;
  /** POST /api/merge-train/loop/run: a dry run answers its report; a real one starts in the background. */
  runMergeLoop(dryRun: boolean): Promise<{ started: boolean; report?: MergeLoopReport }>;
  redTeamSchedules(): Promise<RedTeamSchedule[]>;
  createRedTeamSchedule(body: NewRedTeamSchedule): Promise<RedTeamSchedule>;
  updateRedTeamSchedule(id: string, body: NewRedTeamSchedule): Promise<RedTeamSchedule>;
  deleteRedTeamSchedule(id: string): Promise<void>;
  probeHunter(id: string): Promise<HunterProbe>;
}

export const loopsHttp: LoopsApi = {
  redTeamRuns: () => request("/api/redteam/runs"),
  startRedTeamRun: (body) => post("/api/redteam/runs", body),
  stopRedTeamRun: (id) => post(`/api/redteam/runs/${enc(id)}/stop`),
  synthesizeRedTeamRun: (id) => post(`/api/redteam/runs/${enc(id)}/synthesize`),
  loops: () => request("/api/loops"),
  createLoop: (body) => post("/api/loops", body),
  updateLoop: (id, body) => put(`/api/loops/${enc(id)}`, body),
  deleteLoop: (id) => del(`/api/loops/${enc(id)}`),
  runLoopNow: (id) => post(`/api/loops/${enc(id)}/run-now`),
  loopRuns: (id) => request(`/api/loops/${enc(id)}/runs`),
  runDiskCleanup: (id, dryRun) => post(`/api/loops/${enc(id)}/run-now${dryRun ? "?dry_run=1" : ""}`),
  docsLoop: () => request("/api/docs-loop"),
  saveDocsLoop: (settings) => put("/api/docs-loop", settings),
  setDocsLoopTarget: (target, enabled) => post(`/api/docs-loop/${enabled ? "enable" : "disable"}`, { target }),
  runDocsLoop: (dryRun) => post("/api/docs-loop/run", { dry_run: dryRun }),
  tsAnyLoop: () => request("/api/ts-any-loop"),
  saveTsAnyLoop: (settings) => put("/api/ts-any-loop", settings),
  runTsAnyLoop: (body) => post("/api/ts-any-loop/run", body),
  mergeTrain: () => request("/api/merge-train"),
  supplyChainLoop: () => request("/api/supply-chain-loop"),
  saveSupplyChainLoop: (settings) => put("/api/supply-chain-loop", settings),
  runSupplyChainLoop: (body) => post("/api/supply-chain-loop/run", body),
  mergeLoop: () => request("/api/merge-train/loop"),
  saveMergeLoop: (settings) => put("/api/merge-train/loop", settings),
  runMergeLoop: (dryRun) => post(`/api/merge-train/loop/run${dryRun ? "?dry_run=true" : ""}`),
  redTeamSchedules: () => request("/api/redteam/schedules"),
  createRedTeamSchedule: (body) => post("/api/redteam/schedules", body),
  updateRedTeamSchedule: (id, body) => put(`/api/redteam/schedules/${enc(id)}`, body),
  deleteRedTeamSchedule: (id) => del(`/api/redteam/schedules/${enc(id)}`),
  probeHunter: (id) => request(`/api/hunters/${enc(id)}/probe`),
};
