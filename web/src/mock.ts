// In-browser mock of the harness API and event streams, enabled with `?mock=1` (issue #827).
// A thin barrel: the shared, per-call state is built by ./mockState and each feature's methods live
// in ./features/<feature>/mock.ts.
import type { Api } from "./api";
import { createMockState } from "./mockState";
import { hostMock } from "./features/host/mock";
import { builtWithMock } from "./features/builtWith/mock";
import { fleetMock } from "./features/fleet/mock";
import { modulesMock } from "./features/modules/mock";
import { providersMock } from "./features/providers/mock";
import { remoteMock } from "./features/remote/mock";
import { reposMock } from "./features/repos/mock";
import { sessionsMock } from "./features/sessions/mock";
import { orgsMock } from "./features/orgs/mock";
import { memoryMock } from "./features/memory/mock";
import { chatMock } from "./features/chat/mock";
import { loopsMock } from "./features/loops/mock";
import { eventsMock } from "./features/events/mock";
import { historyMock } from "./features/history/mock";
import { handoffMock } from "./features/handoff/mock";
import { decisionsMock } from "./features/decisions/mock";
import { modelsMock } from "./features/models/mock";

export { mockSplitLabels, mockDrafts, DEMO_MAP } from "./features/repos/mock";

/** The whole mock API: one fresh state object shared by every feature's methods. */
export function createMockApi(): Api {
  const ms = createMockState();
  return {
    mock: true,
    ...hostMock(ms),
    ...builtWithMock(),
    ...fleetMock(ms),
    ...modulesMock(ms),
    ...providersMock(ms),
    ...remoteMock(ms),
    ...reposMock(ms),
    ...sessionsMock(ms),
    ...orgsMock(ms),
    ...memoryMock(ms),
    ...chatMock(ms),
    ...loopsMock(ms),
    ...eventsMock(ms),
    ...historyMock(ms),
    ...handoffMock(ms),
    ...decisionsMock(ms),
    ...modelsMock(ms),
  };
}
