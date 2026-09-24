// What each colony that needs you is actually asking. The session list only says a colony is
// `waiting_for_answer`; the question itself is in its event log, and GET /api/sessions/{id} already
// reduces that to a one-line diagnosis ("waiting for an answer: <question>"). This reads it for every
// colony that needs you — refetched when the colony moves, not on a timer — so the inbox, the
// needs-you lists and the nest can say what is being asked instead of only that something is.
import { useContext, useEffect, useMemo, useState } from "react";

import { ApiContext } from "../context";
import { needsYou } from "../notifications";
import type { Diagnosis, Session } from "../types";

const PREFIX = /^waiting for an answer:\s*/i;

/** The question in a diagnosis, without its "waiting for an answer:" lead; null when it names none. */
export function questionOf(diagnosis: Diagnosis | null | undefined): string | null {
  if (!diagnosis || diagnosis.state !== "waiting_on_human") return null;
  if (!PREFIX.test(diagnosis.text)) return null;
  const text = diagnosis.text.replace(PREFIX, "").trim();
  return text || null;
}

/** Whether a colony's attention flag is the watchdog's own (a stall), not just its open question. */
export function watchdogFlagged(session: Session): boolean {
  const reason = (session.attention as { reason?: string } | null | undefined)?.reason;
  return Boolean(session.attention) && reason !== "waiting_for_answer";
}

/** The key a colony's question is refetched on: it only changes when the colony moves. */
const keyOf = (s: Session) => `${s.id}:${s.status}:${s.last_activity_at ?? s.updated_at}`;

/** Colony id → what it is asking, for every colony that needs you. */
export function useOpenQuestions(sessions: readonly Session[]): Readonly<Record<string, string>> {
  // Optional on purpose: a view rendered without the API (static tests) just shows no questions.
  const api = useContext(ApiContext);
  const waiting = useMemo(() => sessions.filter(needsYou), [sessions]);
  const signature = waiting.map(keyOf).join("|");
  const [questions, setQuestions] = useState<Record<string, string>>({});

  useEffect(() => {
    if (!api) return;
    let active = true;
    for (const s of waiting) {
      api.session(s.id).then(
        (full) => {
          if (!active) return;
          const q = questionOf(full.diagnosis);
          setQuestions((current) => {
            if (q === null) {
              if (!(s.id in current)) return current;
              const next = { ...current };
              delete next[s.id];
              return next;
            }
            return current[s.id] === q ? current : { ...current, [s.id]: q };
          });
        },
        () => {
          /* a courtesy: without it the row says the colony is waiting, as before */
        },
      );
    }
    return () => {
      active = false;
    };
    // The signature is the dependency: a refetch only when a waiting colony appears or moves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, signature]);

  // Only the colonies still waiting; one that moved on drops out on the next render.
  return useMemo(() => {
    const out: Record<string, string> = {};
    for (const s of waiting) if (questions[s.id]) out[s.id] = questions[s.id];
    return out;
  }, [waiting, questions]);
}
