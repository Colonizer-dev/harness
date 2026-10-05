// The hand-off feature's mock methods (issue #738). It validates an upload the way the server does
// (owner/name, a messages array, the 2 MiB cap) and creates a colony the way the sessions mock does,
// so the demo build's hand-off flow exercises the same checks.
import { ApiError } from "../../http";
import { clone, sleep } from "../../mockShared";
import { MockSession, baseSession } from "../sessions/mockSession";
import { MAX_HANDOFF_BYTES, asSimpleTranscript, repoValid, transcriptBytes } from "./handoff";
import type { MockState } from "../../mockState";
import type { HandoffApi } from "./api";

export function handoffMock(ms: MockState): HandoffApi {
  return {
    handoffSession: async (body) => {
      await sleep(350);
      if (!repoValid(body.repo)) throw new ApiError("repo must be owner/name", 400);
      const transcript = asSimpleTranscript(body.transcript);
      if (!transcript) throw new ApiError("the upload has no messages array", 400);
      if (transcriptBytes(JSON.stringify(transcript)) > MAX_HANDOFF_BYTES) throw new ApiError("the upload is larger than 2 MiB", 413);
      const id = ms.mockId();
      const title = body.title?.trim() || transcript.title?.trim() || "Session hand-off";
      const session = new MockSession(
        { ...baseSession(id, body.repo, null, title), base: body.branch?.trim() || "main" },
        false,
        body.instructions ?? null,
      );
      ms.sessions.set(id, session);
      ms.colonyActivity("colony.launch", session.session);
      return clone(session.session);
    },
    sessionHandoff: async (id) => {
      await sleep(180);
      const s = ms.find(id).session;
      // A small but honest transcript: the colony's title in, its last summary out.
      return {
        id: s.id,
        timestamp: s.created_at,
        git_branch: s.branch,
        title: s.issue_title,
        messages: [
          { role: "user", content: s.issue_title || "Continue this colony locally." },
          { role: "assistant", content: [{ type: "text", text: s.summary ?? "This colony had no recorded summary." }] },
        ],
      };
    },
  };
}
