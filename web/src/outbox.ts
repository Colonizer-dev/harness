// The outbox's page half (issue #746): when a colony's events socket is down, a command the cockpit
// cannot send becomes an HTTP call the service worker queues and retries — the answer twin and
// messages endpoints take plain cookie-authed POSTs, so the queue needs no token in any url. The
// queue itself and the delivery rules live in the worker (public/sw-outbox.js); this file builds
// the items, hands them over, and surfaces the worker's progress reports to the UI.
import { useEffect, useState } from "react";

import type { ClientCommand, Question } from "./types";

/** One queued command, as the worker stores it: a cookie-authed POST of `body` (JSON) to `url`. */
export interface OutboxItem {
  id: string;
  url: string;
  body: string;
  queuedAt: number;
  /** Stamped by the worker at enqueue, so two items from the same millisecond still keep their order. */
  seq?: number;
}

/** What the worker broadcasts after every change: the queue size, and what the last flush did. */
export interface OutboxStatus {
  pending: number;
  delivered: string[];
  dropped: { id: string; status: number }[];
}

/** The part of a SessionStream the queueing decision needs, so tests can pass a stub. */
export type Sender = { readonly sessionId: string; send(command: ClientCommand): boolean } | null;

/**
 * A fresh item id, which doubles as the messages endpoint's dedupe id: 1–64 chars of
 * [A-Za-z0-9_-], so the server's repeat check sees the same value on every retry.
 */
export function outboxId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `q${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * The HTTP twin of a command worth queueing, or null for the commands that are not: an interrupt
 * or a model switch queued behind a dead socket would fire long after the moment it meant.
 * The answer twin takes the socket command's fields plus the questions the operator actually read:
 * the mothership refuses the replay (409) when the question under that id has changed meanwhile,
 * and once one replay lands the question is closed, so a second is refused too — exactly once.
 * A queued message carries the item id as its dedupe id, so a retry after a half-delivered flush
 * cannot double-post it.
 */
export function commandToItem(command: ClientCommand, sessionId: string, id: string, queuedAt: number, questions?: readonly Question[]): OutboxItem | null {
  const session = encodeURIComponent(sessionId);
  if (command.type === "answer") {
    const body = { question_id: command.question_id, answers: command.answers, response: command.response, ...(questions ? { questions } : {}) };
    return { id, url: `/api/sessions/${session}/answer`, body: JSON.stringify(body), queuedAt };
  }
  if (command.type === "user_message") {
    return { id, url: `/api/sessions/${session}/messages`, body: JSON.stringify({ id, text: command.text }), queuedAt };
  }
  return null;
}

/** The Background Sync tag the worker flushes on; also the message type the page nudges with. */
export const OUTBOX_SYNC_TAG = "colonizer-outbox";

/** Whether queueing is on the table at all: a service-worker controller exists to hand items to. */
export function canQueue(nav: { serviceWorker?: { controller: unknown } | null } | undefined = typeof navigator === "undefined" ? undefined : navigator): boolean {
  return Boolean(nav?.serviceWorker?.controller);
}

/** One-shot sync registration; browsers without Background Sync just get the page nudges. */
function registerSync(): void {
  navigator.serviceWorker.ready
    .then((registration) =>
      (registration as ServiceWorkerRegistration & { sync?: { register(tag: string): Promise<void> } }).sync?.register(OUTBOX_SYNC_TAG).catch(() => undefined),
    )
    .catch(() => undefined);
}

/** What sendOrQueue did with a command: sent now, queued for the worker (with the item's id, so the
 *  caller can recognise its delivery or its drop in the worker's reports), or failed — no worker
 *  (the dev server, the mock) or a command that must not outlive the moment. */
export type SendOutcome = { status: "sent" } | { status: "queued"; id: string } | { status: "failed" };

/**
 * Sends a command the way the socket does, or queues it for the worker when the socket is down.
 * `questions` is what an answer answers, as the operator saw it — only the queued twin sends it.
 */
export function sendOrQueue(stream: Sender, command: ClientCommand, questions?: readonly Question[]): SendOutcome {
  if (stream?.send(command)) return { status: "sent" };
  const controller = typeof navigator !== "undefined" ? navigator.serviceWorker?.controller : null;
  if (!controller || !stream) return { status: "failed" };
  const item = commandToItem(command, stream.sessionId, outboxId(), Date.now(), questions);
  if (!item) return { status: "failed" };
  controller.postMessage({ type: "colonizer:outbox-enqueue", item });
  registerSync();
  return { status: "queued", id: item.id };
}

/** Why a dropped item was dropped, as the toast reads it. A 409 usually means the thing the user
 *  wanted already happened — the question is no longer open, the colony is in no mood for messages
 *  — so the wording stays calm about it instead of crying error. */
export function droppedText(kind: "answer" | "message", status: number): string {
  if (status === 409) {
    return kind === "answer"
      ? "The question was no longer open, or had changed — it may already have been answered."
      : "The colony isn't taking messages right now, so the queued message wasn't sent.";
  }
  if (status === 404) return "The colony is gone.";
  if (status === 0) return `It waited offline too long, so the queued ${kind} was not sent.`;
  return `The mothership refused it (${status}).`;
}

/**
 * Tracks the worker's progress reports, and gives the queue its nudge without Background Sync:
 * flush on mount, on `online`, and on returning to the tab — the retry-on-next-open fallback.
 */
export function useOutbox(): OutboxStatus {
  const [status, setStatus] = useState<OutboxStatus>({ pending: 0, delivered: [], dropped: [] });
  useEffect(() => {
    const container = navigator.serviceWorker;
    const controller = container?.controller;
    const nudge = () => controller?.postMessage({ type: "colonizer:outbox-flush" });
    if (!container) return;
    const onMessage = (event: MessageEvent) => {
      if (event.origin !== window.location.origin) return;
      const data = event.data as Partial<OutboxStatus> & { type?: string } | null;
      if (data?.type !== "colonizer:outbox") return;
      setStatus({
        pending: typeof data.pending === "number" ? data.pending : 0,
        delivered: Array.isArray(data.delivered) ? data.delivered : [],
        dropped: Array.isArray(data.dropped) ? data.dropped : [],
      });
    };
    const onVisible = () => {
      if (document.visibilityState === "visible") nudge();
    };
    container.addEventListener("message", onMessage);
    window.addEventListener("online", nudge);
    document.addEventListener("visibilitychange", onVisible);
    nudge();
    return () => {
      container.removeEventListener("message", onMessage);
      window.removeEventListener("online", nudge);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);
  return status;
}
