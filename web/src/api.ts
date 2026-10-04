// Typed client for the harness browser API. A feature's types, calls and mock live together in
// src/features/<feature>/ (types.ts, api.ts, mock.ts, mockState.ts); this file only re-exports them
// and composes `Api`. Wiring a feature costs one line each in types.ts, api.ts, mock.ts and mockState.ts.
import { DEMO } from "./demo";
import { hostHttp, type HostApi } from "./features/host/api";
import { fleetHttp, type FleetApi } from "./features/fleet/api";
import { modulesHttp, type ModulesApi } from "./features/modules/api";
import { providersHttp, type ProvidersApi } from "./features/providers/api";
import { remoteHttp, type RemoteApi } from "./features/remote/api";
import { reposHttp, type ReposApi } from "./features/repos/api";
import { sessionsHttp, type SessionsApi } from "./features/sessions/api";
import { orgsHttp, type OrgsApi } from "./features/orgs/api";
import { memoryHttp, type MemoryApi } from "./features/memory/api";
import { chatHttp, type ChatApi } from "./features/chat/api";
import { loopsHttp, type LoopsApi } from "./features/loops/api";
import { eventsHttp, type EventsApi } from "./features/events/api";
import { historyHttp, type HistoryApi } from "./features/history/api";

export { ApiError, SOCKET_OPEN, splitNdjson } from "./http";
export type { SocketLike } from "./http";
export { holdsIssue, heldByFor, isEpic, epicMarker, heldInBatch, claimWaitersFor, claimWaitPosition } from "./features/repos/claims";
export type { SaveModuleRequest } from "./features/modules/api";
export type { BehindInfo, CatchUpResult, StopReply } from "./features/sessions/api";

export interface Api extends HostApi, FleetApi, ModulesApi, ProvidersApi, RemoteApi, ReposApi, SessionsApi, OrgsApi, MemoryApi, ChatApi, LoopsApi, EventsApi, HistoryApi {
  readonly mock: boolean;
}

export const httpApi: Api = {
  mock: false,
  ...hostHttp,
  ...fleetHttp,
  ...modulesHttp,
  ...providersHttp,
  ...remoteHttp,
  ...reposHttp,
  ...sessionsHttp,
  ...orgsHttp,
  ...memoryHttp,
  ...chatHttp,
  ...loopsHttp,
  ...eventsHttp,
  ...historyHttp,
};

/** `?mock=1` swaps in an in-browser backend so the UI can be exercised without a harness; the demo build forces it on. */
export async function loadApi(): Promise<Api> {
  if (DEMO || new URLSearchParams(location.search).get("mock") === "1") {
    const { createMockApi } = await import("./mock");
    return createMockApi();
  }
  return httpApi;
}
