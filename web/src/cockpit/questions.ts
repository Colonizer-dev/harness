// What each colony that needs you is actually asking. The session list only says a colony is
// `waiting_for_answer`; the question itself is in its event log, and GET /api/sessions/{id} already
// reduces that to a one-line diagnosis ("waiting for an answer: <question>"). This reads it for every
// colony that needs you — refetched when the colony moves, not on a timer — so the inbox, the
// needs-you lists and the nest can say what is being asked instead of only that something is.
import { useContext, useEffect, useMemo, useState } from "react";

import { attentionText } from "../components/ui";
import { ApiContext } from "../context";
import { needsYou } from "../notifications";
import type { Attention, Diagnosis, Session } from "../types";

const PREFIX = /^waiting for an answer:\s*/i;

/** The question in a diagnosis, without its "waiting for an answer:" lead; null when it names none. */
export function questionOf(diagnosis: Diagnosis | null | undefined): string | null {
  if (!diagnosis || diagnosis.state !== "waiting_on_human") return null;
  if (!PREFIX.test(diagnosis.text)) return null;
  const text = diagnosis.text.replace(PREFIX, "").trim();
  return text || null;
}

/** The attention reasons the watchdog itself raises: a stall, a stall it ran out of nudges on, a control defeat. */
const WATCHDOG_REASONS: ReadonlySet<string> = new Set(["stalled", "nudges_exhausted", "control_defeat"]);

/** Whether a colony's attention flag is the watchdog's own (a stall), not its open question or an autopilot hold. */
export function watchdogFlagged(session: Session): boolean {
  const reason = (session.attention as { reason?: string } | null | undefined)?.reason;
  return reason != null && WATCHDOG_REASONS.has(reason);
}

/** Whether the colony is, or may be, waiting on an answer: only then may a card ask for one (issue #1093). */
export function expectsAnswer(session: Session): boolean {
  return session.status === "waiting_for_answer" || session.attention?.reason === "waiting_for_answer" || Boolean(session.prewarm);
}

/**
 * What a colony that needs you needs, in one line, for when its question text is not (or not yet)
 * known. Names the real cause (issue #1093): the watchdog only when the watchdog flagged it, a
 * question only when one is expected, and otherwise the attention's own words — "Stopped on repeated
 * gateway errors (502, connection to Anthropic)" rather than "the watchdog flagged this colony".
 */
export function needsYouLine(session: Session, asked = "the colony asked you a question"): string {
  if (watchdogFlagged(session)) return "the watchdog flagged this colony";
  if (expectsAnswer(session)) return asked;
  if (session.attention) return attentionText(session.attention);
  if (session.status === "failed") return "the colony failed";
  return asked;
}

/** Whether the colony is parked while an automatic retry of a provider error backs off (issues #980, #1093). */
export function autoRetrying(session: Session): boolean {
  return session.status === "parked" && session.attention?.reason === "provider_retry";
}

/** Whether autopilot holds the colony because its gateway retries ran out (issue #1093): Retry is the way on. */
export function gatewayHeld(session: Session): boolean {
  return session.attention?.reason === "autopilot_held" && session.attention.cause === "gateway_error";
}

/**
 * The pending retry, as the card says it: "Stopped on a model gateway error (502, connection to
 * Anthropic): retrying in 4 min". A missing or unreadable `retry_at` (an older mothership) still
 * says the retry is automatic.
 */
export function retryLine(attention: Attention, now: number = Date.now()): string {
  const summary = attention.summary?.trim() || "Stopped on a model gateway error";
  const at = attention.retry_at ? Date.parse(attention.retry_at) : Number.NaN;
  if (Number.isNaN(at)) return `${summary}: retrying automatically`;
  const minutes = Math.ceil((at - now) / 60_000);
  return minutes <= 0 ? `${summary}: retrying now` : `${summary}: retrying in ${minutes} min`;
}

/** The message Retry sends a colony held on gateway errors: the work is fine, the request is what failed. */
export const GATEWAY_RETRY_MESSAGE =
  "Your last turn stopped on a model gateway error. Nothing was wrong with your work: continue where you left off.";

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
