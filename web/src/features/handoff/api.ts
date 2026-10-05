// The hand-off feature's API methods (issue #738). The root `Api` interface composes `HandoffApi`.
import { enc, post, request } from "../../http";
import type { Session } from "../sessions/types";
import type { HandoffRequest, SimpleTranscript } from "./types";

export interface HandoffApi {
  /** POST /api/handoff: start a colony from an uploaded local session's Simple JSON (400/413 with the reason). */
  handoffSession(body: HandoffRequest): Promise<Session>;
  /** GET /api/sessions/{id}/handoff: the colony's conversation as Simple JSON (served as `colony.json`). */
  sessionHandoff(id: string): Promise<SimpleTranscript>;
}

export const handoffHttp: HandoffApi = {
  handoffSession: (body) => post("/api/handoff", body),
  sessionHandoff: (id) => request(`/api/sessions/${enc(id)}/handoff`),
};
