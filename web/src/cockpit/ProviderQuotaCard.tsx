// The "Provider out of quota" card (issue #767): one per provider whose plan ran out, naming the
// provider and model, when the plan resets, and every colony blocked on it — with three answers.
// Switch moves those colonies (or their orgs, or every role on the provider install-wide) to a
// healthy model and restarts them on it, and says what each setting was; Wait parks
// them and the mothership resumes them at the reset, counted down here; Stop stops them. It is a
// dedicated card, not a free-form question from an agent: the agents cannot answer this themselves.
//
// The words and the request bodies are pure functions, so the tests (no DOM) pin them directly and
// render the card to static markup.
import { useEffect, useState, type ReactElement } from "react";

import type { QuotaActionReply, QuotaActionRequest, QuotaAlternative, QuotaCard, QuotaChange } from "../types";
import { Button, cx, inputClass } from "../components/ui";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** `Oct 1, 16:00 UTC`: the reset as an absolute time, in UTC so every maintainer reads the same. */
export function formatResetUtc(unix: number): string {
  const d = new Date(unix * 1000);
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const mm = String(d.getUTCMinutes()).padStart(2, "0");
  return `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}, ${hh}:${mm} UTC`;
}

/** `2d 9h`, `3h 12m`, `4m`, `now`: how long until `unix`, from `nowMs`. */
export function formatCountdown(unix: number, nowMs: number): string {
  const seconds = Math.floor(unix - nowMs / 1000);
  if (seconds <= 0) return "now";
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return minutes > 0 ? `${minutes}m` : "under a minute";
}

/**
 * The card's header: `bailian · qwen3.8-max is out of quota. Resets Oct 1, 16:00 UTC (in 2d 9h)`.
 * A reset the provider named only in words (no timestamp) is quoted; none at all says so.
 */
export function quotaCardHeader(card: QuotaCard, nowMs: number): string {
  const model = card.models[0];
  const what = model ? `${card.provider} · ${model} is out of quota.` : `${card.provider} is out of quota.`;
  if (card.reset_unix != null) {
    return `${what} Resets ${formatResetUtc(card.reset_unix)} (in ${formatCountdown(card.reset_unix, nowMs)})`;
  }
  if (card.reset_at) return `${what} Resets ${card.reset_at}`;
  return `${what} No reset time given; the mothership re-checks every 15 minutes`;
}

/** The waiting line under the header while colonies are parked for the reset. */
export function quotaWaitingLine(card: QuotaCard, nowMs: number): string | null {
  if (card.waiting === 0) return null;
  const who = card.waiting === 1 ? "1 colony waits" : `${card.waiting} colonies wait`;
  if (card.resume_unix == null) return `${who} for the provider to recover`;
  return `${who} — resuming in ${formatCountdown(card.resume_unix, nowMs)}`;
}

/** One picker option: `sonnet — Claude Sonnet (latest) · healthy`, `glm-5 · Z.AI — 12.5% failing`. */
export function alternativeLabel(alt: QuotaAlternative): string {
  if (!alt.healthy) return `${alt.label} — degraded, ${alt.failure_pct}% failing`;
  if (alt.rated) return `${alt.label} — healthy, ${alt.failure_pct}% failing`;
  return `${alt.label} — healthy`;
}

/** The first healthy model on offer, the picker's default. */
export function defaultAlternative(card: QuotaCard): string {
  return (card.alternatives.find((a) => a.healthy) ?? card.alternatives[0])?.id ?? "";
}

export type SwitchScope = "colonies" | "org" | "all";

/**
 * Whether "remember as fallback" can apply to this model: any Claude model (the colony's router
 * retries it), or a model on another provider that speaks the card's provider's wire (the gateway
 * retries it there). A cross-wire model cannot be a fallback.
 */
export function canRemember(card: QuotaCard, model: string): boolean {
  if (model === "") return false;
  if (!model.includes("/")) return true;
  const alt = card.alternatives.find((a) => a.id === model);
  return alt?.wire != null && alt.wire === (card.wire ?? "anthropic");
}

/** The switch request; the caller passes `remember` only where [`canRemember`] allows it. */
export function switchRequest(model: string, scope: SwitchScope, remember: boolean): QuotaActionRequest {
  const body: QuotaActionRequest = { action: "switch", model, scope };
  if (remember) body.remember = true;
  return body;
}

/** Where each change landed, in words: `install model`, `org beta subagent_model`, `bailian fallback`. */
function changeWhere(change: QuotaChange): string {
  switch (change.scope) {
    case "install":
      return `install ${change.key}`;
    case "org":
      return `org ${change.target} ${change.key}`;
    case "colony":
      return `colony ${change.target} ${change.key}`;
    case "provider":
      return `${change.target} fallback`;
  }
}

/** One line per setting a switch changed: `install model: was bailian/qwen3.8-max → now zai/glm-5`. */
export function quotaChangeLines(reply: QuotaActionReply): string[] {
  return (reply.changes ?? []).map((c) => `${changeWhere(c)}: was ${c.was ?? "unset"} → now ${c.now}`);
}

/** The toast after an action: what happened, and what did not. */
export function quotaActionSummary(reply: QuotaActionReply): string {
  const done = reply.colonies.length;
  const verb = reply.action === "switch" ? "switched" : reply.action === "wait" ? "parked until the reset" : "stopped";
  const settings = (reply.changes ?? []).filter((c) => c.scope !== "colony").length;
  const head =
    `${reply.provider}: ${done} ${done === 1 ? "colony" : "colonies"} ${verb}` +
    (settings > 0 ? `, ${settings} ${settings === 1 ? "setting" : "settings"} changed` : "");
  if (reply.failed.length === 0) return head;
  return `${head}; ${reply.failed.length} could not be: ${reply.failed.map((f) => `${f.id} (${f.error})`).join(", ")}`;
}

/** Counts down once a minute while the card shows, so the reset and the waiting line stay current. */
function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, []);
  return now;
}

export function ProviderQuotaCard({
  card,
  onAction,
  onOpenColony,
  nowMs,
}: {
  card: QuotaCard;
  /** Sends the action; the caller toasts the reply and refreshes. */
  onAction: (provider: string, body: QuotaActionRequest) => Promise<unknown>;
  onOpenColony?: (id: string) => void;
  /** Fixed clock for tests; the live card ticks on its own. */
  nowMs?: number;
}): ReactElement {
  const ticking = useNow();
  const now = nowMs ?? ticking;
  const [model, setModel] = useState(() => defaultAlternative(card));
  const [scope, setScope] = useState<SwitchScope>("colonies");
  const [remember, setRemember] = useState(false);
  const [busy, setBusy] = useState<QuotaActionRequest["action"] | null>(null);
  const run = async (body: QuotaActionRequest) => {
    if (busy) return;
    setBusy(body.action);
    try {
      await onAction(card.provider, body);
    } finally {
      setBusy(null);
    }
  };
  const waiting = quotaWaitingLine(card, now);
  const count = card.colonies.length;
  return (
    <div
      role="group"
      aria-label={`Provider out of quota: ${card.provider}`}
      className="rounded-md border border-warn bg-warn-soft px-3.5 py-3 text-body-sm text-text"
    >
      <div className="font-mono text-meta-lg text-warn">Provider out of quota</div>
      <div className="mt-1 text-lead font-semibold [text-wrap:pretty]">{quotaCardHeader(card, now)}</div>
      {waiting && <div className="mt-1 text-small-lg text-warn">{waiting}</div>}
      <div className="mt-2 text-small-lg text-muted">
        {count} {count === 1 ? "colony" : "colonies"} blocked
        {card.orgs.length > 0 ? ` in ${card.orgs.join(", ")}` : ""}:
      </div>
      <ul className="m-0 mt-1 list-none p-0">
        {card.colonies.map((colony) => (
          <li key={colony.id} className="flex items-center gap-2 py-0.5 font-mono text-small">
            <button
              type="button"
              onClick={() => onOpenColony?.(colony.id)}
              className="cursor-pointer border-0 bg-transparent p-0 text-left text-text hover:underline"
            >
              {colony.repo}
              {colony.issue != null ? `#${colony.issue}` : ""}
            </button>
            <span className="text-faint">
              {colony.waiting ? "waiting for the reset" : colony.status}
              {colony.hits ? ` · ${colony.hits} quota ${colony.hits === 1 ? "answer" : "answers"}` : ""}
            </span>
          </li>
        ))}
      </ul>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <label className="sr-only" htmlFor={`quota-model-${card.provider}`}>
          Switch to model
        </label>
        <select
          id={`quota-model-${card.provider}`}
          value={model}
          onChange={(e) => setModel(e.target.value)}
          className={cx(inputClass, "h-8 w-auto max-w-[22rem] text-small-lg")}
        >
          {card.alternatives.map((alt) => (
            <option key={alt.id} value={alt.id} disabled={!alt.healthy}>
              {alternativeLabel(alt)}
            </option>
          ))}
        </select>
        <label className="sr-only" htmlFor={`quota-scope-${card.provider}`}>
          Apply to
        </label>
        <select
          id={`quota-scope-${card.provider}`}
          value={scope}
          onChange={(e) => setScope(e.target.value as SwitchScope)}
          className={cx(inputClass, "h-8 w-auto text-small-lg")}
        >
          <option value="colonies">these colonies</option>
          <option value="org">this org ({card.orgs.join(", ") || "none"})</option>
          <option value="all">every role using {card.provider}</option>
        </select>
        <Button
          size="sm"
          variant="primary"
          disabled={busy !== null || model === ""}
          onClick={() => void run(switchRequest(model, scope, remember && canRemember(card, model)))}
        >
          {busy === "switch" ? "Switching…" : "Switch model"}
        </Button>
        <label
          className={cx("flex items-center gap-1.5 text-small", canRemember(card, model) ? "text-muted" : "text-faint")}
          title="Save it as this provider's fallback_model, so the next time its plan runs out colonies retry on it by themselves. A Claude model, or a model on a provider that speaks the same wire."
        >
          <input
            type="checkbox"
            checked={remember && canRemember(card, model)}
            disabled={!canRemember(card, model)}
            onChange={(e) => setRemember(e.target.checked)}
          />
          remember as {card.provider}'s fallback
        </label>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <Button size="sm" disabled={busy !== null} onClick={() => void run({ action: "wait" })}>
          {busy === "wait" ? "Parking…" : "Wait until reset"}
        </Button>
        <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => void run({ action: "stop" })}>
          {busy === "stop" ? "Stopping…" : count === 1 ? "Stop it" : `Stop all ${count}`}
        </Button>
        {card.fallback_model && <span className="text-small text-faint">fallback: {card.fallback_model}</span>}
      </div>
    </div>
  );
}

/**
 * Sends a card's action through `send` and says what happened through `say` — the one wiring the
 * inbox and the provider settings share, pinned by the tests without a DOM.
 */
export async function runQuotaAction(
  send: (provider: string, body: QuotaActionRequest) => Promise<QuotaActionReply>,
  say: (message: string, tone?: "error") => void,
  provider: string,
  body: QuotaActionRequest,
): Promise<QuotaActionReply | null> {
  try {
    const reply = await send(provider, body);
    say(quotaActionSummary(reply), reply.failed.length > 0 ? "error" : undefined);
    return reply;
  } catch (e) {
    say(e instanceof Error ? e.message : String(e), "error");
    return null;
  }
}

/**
 * What a switch changed, "was X → now Y" per setting, kept on screen after the card itself goes
 * (its colonies are no longer blocked). Nothing when the reply changed no setting.
 */
export function QuotaChangeSummary({
  reply,
  onDismiss,
}: {
  reply: QuotaActionReply | null;
  onDismiss?: () => void;
}): ReactElement | null {
  const lines = reply ? quotaChangeLines(reply) : [];
  if (!reply || lines.length === 0) return null;
  return (
    <div
      role="status"
      aria-label={`Switched ${reply.provider}`}
      className="rounded-md border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-text"
    >
      <div className="flex items-center gap-2">
        <span className="font-medium">{quotaActionSummary(reply)}</span>
        {onDismiss && (
          <button
            type="button"
            onClick={onDismiss}
            className="ml-auto cursor-pointer border-0 bg-transparent p-0 text-small text-muted hover:underline"
          >
            Dismiss
          </button>
        )}
      </div>
      <ul className="m-0 mt-1 list-none p-0 font-mono text-small text-muted">
        {lines.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
    </div>
  );
}

/** Whether an `onAction` result is a quota reply (the wiring may also resolve to nothing). */
export function isQuotaReply(value: unknown): value is QuotaActionReply {
  return typeof value === "object" && value !== null && "action" in value && "colonies" in value;
}

/** Colonies a quota card already covers: the inbox shows them on the card, not again as questions. */
export function quotaCardColonyIds(cards: QuotaCard[]): Set<string> {
  return new Set(cards.flatMap((card) => card.colonies.map((c) => c.id)));
}
