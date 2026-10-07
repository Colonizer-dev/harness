import type { ProviderHealth } from "../../types";
import { Spinner, cx } from "../ui";

export type HealthView = { state: "checking" } | { state: "done"; result: ProviderHealth } | { state: "failed"; message: string };

export function HealthStatus({ health, degraded }: { health: HealthView; degraded?: boolean }) {
  if (health.state === "checking") {
    return (
      <span role="status" className="flex items-center gap-1.5 text-small text-muted">
        <Spinner className="size-3" /> Checking from the Mothership…
      </span>
    );
  }
  let tone: "ok" | "warn" | "err";
  let text: string;
  let title: string | undefined;
  if (health.state === "failed") {
    tone = "err";
    text = `Check failed: ${health.message}`;
  } else {
    const r = health.result;
    const latency = r.latency_ms != null ? `${Math.round(r.latency_ms)} ms` : null;
    const models = r.models.length ? `${r.models.length} model${r.models.length === 1 ? "" : "s"}` : null;
    // The plan balance rides along on the probe when the provider has a quota URL configured; its
    // failure is its own clause and never changes the reachability verdict (issue #199).
    const quota = r.quota ? r.quota.error ?? (r.quota.remaining != null ? `${r.quota.remaining.toLocaleString("en-US")} left in plan` : null) : null;
    // A note marks a non-2xx the Mothership judged healthy (an anthropic-wire endpoint with no
    // /v1/models), so it skips the HTTP warning.
    if (!r.reachable) {
      tone = "err";
      text = `Unreachable${r.error ? `: ${r.error}` : ""}`;
    } else if (!r.note && r.status != null && (r.status < 200 || r.status > 299)) {
      tone = "warn";
      text = [`HTTP ${r.status}`, latency, r.error, quota].filter(Boolean).join(" · ");
    } else {
      tone = "ok";
      // A passing probe is one request; say so next to a provider failing a share of its real traffic.
      text = ["Reachable", latency, models ?? r.note, quota, degraded ? "but failing real traffic" : null].filter(Boolean).join(" · ");
    }
    title = [
      r.models.length ? `Models: ${r.models.join(", ")}` : r.note ? "The endpoint does not list its models; requests route normally." : null,
      r.checked_at ? `Checked ${new Date(r.checked_at).toLocaleTimeString()}` : null,
    ]
      .filter(Boolean)
      .join("\n");
  }
  return (
    <span
      role="status"
      title={title || undefined}
      className={cx("flex items-start gap-1.5 text-small font-medium", tone === "ok" ? "text-ok" : tone === "warn" ? "text-warn" : "text-err")}
    >
      <span className="mt-[5px] size-1.5 shrink-0 rounded-full bg-current" />
      <span className="min-w-0 [overflow-wrap:anywhere]">{text}</span>
    </span>
  );
}
