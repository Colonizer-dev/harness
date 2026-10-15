import { useCallback, useEffect, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { LivingBrainBackfill, LivingBrainDetail, LivingBrainDevice, LivingBrainQueue, LivingBrainRule, LivingBrainState, OrgInfo } from "../../types";
import { Badge, Button, Spinner, Switch, cx, inputClass, timeAgo } from "../ui";
import { ConnectionCard, Pane } from "./ui";

// ---------------------------------------------------------------------------
// Settings → Living Brain (crates/colonizer/src/living_brain): connect the team brain, choose which
// orgs and repositories send their colony sessions and get its MCP server, and see the send queue.
// The token never reaches the browser: the mothership runs the device sign-in and keeps it.
// ---------------------------------------------------------------------------

export const DETAIL_LABEL: Record<LivingBrainDetail, string> = {
  full: "Full — messages, reasoning, tool inputs and outputs",
  messages: "Messages — every message, tool names only",
  summary: "Summary — messages cut to 2,000 characters, tool names only",
};

/** The connection in one line, for the card's header. */
export function livingBrainStatusText(s: LivingBrainState): string {
  if (s.state === "disconnected") return "Not connected.";
  if (s.state === "unauthorized") return "Living Brain refused the saved token — it was revoked or expired. Connect again.";
  const who = s.token_name ? `token “${s.token_name}”` : "a personal access token";
  return `Connected with ${who}${s.token_prefix ? ` (${s.token_prefix})` : ""}${s.connected_at ? `, ${timeAgo(s.connected_at)}` : ""}.`;
}

/** The queue in one line. */
export function queueText(q: LivingBrainQueue): string {
  const parts = [`${q.sent} sent`, `${q.pending} pending`];
  if (q.failed > 0) parts.push(`${q.failed} failed`);
  const sync = q.last_sync ? ` Last sync ${timeAgo(q.last_sync)}.` : " Nothing sent yet.";
  return `${parts.join(", ")}.${sync}`;
}

/** The device sign-in: the code to type and the page to type it on. */
export function DeviceCode({ device, onCancel }: { device: LivingBrainDevice; onCancel: () => void }) {
  const url = device.verification_uri_complete ?? device.verification_uri;
  if (device.state !== "pending") {
    const why: Record<string, string> = {
      connected: "Approved — connected.",
      denied: "The sign-in was denied at Living Brain.",
      expired: "The code expired before it was approved. Connect again for a new one.",
      failed: `The sign-in failed${device.error ? `: ${device.error}` : "."}`,
    };
    return <p className={cx("text-small-lg", device.state === "connected" ? "text-ok" : "text-err")}>{why[device.state]}</p>;
  }
  return (
    <div className="space-y-2 rounded-xl border border-border bg-panel-2 px-3.5 py-3" aria-live="polite">
      <p className="text-small-lg text-muted">Approve this code at Living Brain, signed in as the person whose brain colonies should write to:</p>
      <p className="font-mono text-display-sm font-semibold tracking-widest" data-testid="living-brain-code">
        {device.user_code}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <a className="text-small-lg text-accent underline" href={url} target="_blank" rel="noreferrer noopener">
          Open the sign-in page
        </a>
        <span className="flex items-center gap-1 text-small-lg text-muted">
          <Spinner /> Waiting for approval…
        </span>
        <Button variant="secondary" size="sm" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** One editable rule row. */
export function RuleRow({
  rule,
  mcpAgents,
  busy,
  onChange,
  onRemove,
}: {
  rule: LivingBrainRule;
  mcpAgents: string[];
  busy: boolean;
  onChange: (rule: LivingBrainRule) => void;
  onRemove: () => void;
}) {
  const id = `living-brain-rule-${rule.target.replace(/[^a-zA-Z0-9]/g, "-")}`;
  return (
    <li className="space-y-2 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-body font-medium">{rule.target}</span>
        <Badge tone="neutral">{rule.target.includes("/") ? "repository" : "org"}</Badge>
        <span className="flex-1" />
        <Button variant="secondary" size="sm" disabled={busy} onClick={onRemove}>
          Remove
        </Button>
      </div>
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2">
        <label className="flex items-center gap-2 text-small-lg" htmlFor={`${id}-send`}>
          <Switch id={`${id}-send`} label={`Send ${rule.target} colony sessions`} checked={rule.send} disabled={busy} onChange={(send) => onChange({ ...rule, send })} />
          Send colony sessions
        </label>
        <select
          aria-label={`Detail for ${rule.target}`}
          className={cx(inputClass, "w-auto")}
          value={rule.detail}
          disabled={busy || !rule.send}
          onChange={(e) => onChange({ ...rule, detail: e.target.value as LivingBrainDetail })}
        >
          {(Object.keys(DETAIL_LABEL) as LivingBrainDetail[]).map((d) => (
            <option key={d} value={d}>
              {DETAIL_LABEL[d]}
            </option>
          ))}
        </select>
        <label className="flex items-center gap-2 text-small-lg" htmlFor={`${id}-mcp`}>
          <Switch id={`${id}-mcp`} label={`Give ${rule.target} colonies the brain`} checked={rule.mcp} disabled={busy} onChange={(mcp) => onChange({ ...rule, mcp })} />
          Give colonies the Living Brain MCP server
        </label>
      </div>
      {rule.send && rule.since && <p className="text-small text-faint">Sending colonies active since {new Date(rule.since).toLocaleString()}; older ones wait for “Send existing colonies”.</p>}
      {rule.mcp && <p className="text-small text-faint">Colonies running {mcpAgents.join(" or ")} get the server; other agents start without it.</p>}
    </li>
  );
}

/** The send queue: counts, the last sync and error, and the latest colonies. */
export function QueueStatus({ queue, busy, onRetry }: { queue: LivingBrainQueue; busy: boolean; onRetry: () => void }) {
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <p className="min-w-0 flex-1 text-small-lg text-muted">{queueText(queue)}</p>
        {queue.failed > 0 && (
          <Button variant="secondary" size="sm" disabled={busy} onClick={onRetry}>
            Retry failed
          </Button>
        )}
      </div>
      {queue.last_error && <p className="text-small-lg text-err">Last error: {queue.last_error}</p>}
      {queue.items.length > 0 && (
        <ul className="divide-y divide-border rounded-xl border border-border">
          {queue.items.slice(0, 12).map((item) => (
            <li key={item.colony} className="flex flex-wrap items-center gap-x-2 gap-y-1 px-3 py-2 text-small-lg">
              <Badge tone={item.state === "sent" ? "ok" : item.state === "failed" ? "err" : "info"}>{item.state}</Badge>
              <span className="font-mono">{item.repo}</span>
              <span className="text-faint">{item.colony}</span>
              <span className="flex-1" />
              {item.note_url ? (
                <a className="text-accent underline" href={item.note_url} target="_blank" rel="noreferrer noopener">
                  Open in Living Brain
                </a>
              ) : (
                item.last_error && <span className="text-err [overflow-wrap:anywhere]">{item.last_error}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function LivingBrainPane({ orgs, back }: { orgs?: OrgInfo[]; back?: () => void }) {
  const api = useApi();
  const toast = useToast();
  const [state, setState] = useState<LivingBrainState | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [token, setToken] = useState("");
  const [server, setServer] = useState("");
  const [mcp, setMcp] = useState("");
  const [newTarget, setNewTarget] = useState("");
  const [backfillOrg, setBackfillOrg] = useState("");
  const [count, setCount] = useState<LivingBrainBackfill | null>(null);

  const adopt = useCallback((s: LivingBrainState) => {
    setState(s);
    setServer(s.server_url);
    setMcp(s.mcp_url);
  }, []);

  const refresh = useCallback(() => {
    api
      .livingBrain()
      .then((s) => {
        setLoadFailed(false);
        setState(s);
      })
      .catch(() => setLoadFailed(true));
  }, [api]);

  useEffect(() => {
    api
      .livingBrain()
      .then(adopt)
      .catch(() => setLoadFailed(true));
  }, [api, adopt]);

  // While a sign-in waits, look every two seconds; otherwise keep the queue fresh.
  const waiting = state?.device?.state === "pending";
  useEffect(() => {
    const timer = setInterval(refresh, waiting ? 2000 : 15000);
    return () => clearInterval(timer);
  }, [refresh, waiting]);

  const run = (call: () => Promise<LivingBrainState>, done?: string) => {
    setBusy(true);
    call()
      .then((s) => {
        adopt(s);
        if (done) toast(done, "success");
      })
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => setBusy(false));
  };

  const connect = () => {
    setBusy(true);
    api
      .connectLivingBrain()
      .then((s) => {
        adopt(s);
        const url = s.device?.verification_uri_complete ?? s.device?.verification_uri;
        if (url) window.open(url, "_blank", "noopener");
      })
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => setBusy(false));
  };

  const saveRules = (rules: LivingBrainRule[]) => run(() => api.saveLivingBrain({ rules }));

  const addRule = () => {
    const target = newTarget.trim();
    if (!target || !state) return;
    if (state.rules.some((r) => r.target.toLowerCase() === target.toLowerCase())) {
      toast(`${target} already has a rule.`, "error");
      return;
    }
    saveRules([...state.rules, { target, send: false, detail: "full", mcp: false }]);
    setNewTarget("");
  };

  const backfill = (dryRun: boolean) => {
    if (!backfillOrg.trim()) return;
    setBusy(true);
    api
      .backfillLivingBrain(backfillOrg.trim(), dryRun)
      .then((r) => {
        setCount(dryRun ? r : null);
        if (!dryRun) {
          toast(`${r.queued} colonies of ${r.org} queued for Living Brain.`, "success");
          refresh();
        }
      })
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => setBusy(false));
  };

  const connected = state !== null && state.state !== "disconnected";
  return (
    <Pane
      title="Living Brain"
      subtitle="Your team brain: colony sessions go in, and colonies can read it while they work"
      back={back}
      info={
        <p>
          Living Brain keeps a team's wiki and memory. Connected, the mothership can file each colony's session as an agent log with a
          searchable note under the repository's name — redacted on this machine first (home paths, every saved secret, and the shared
          secret detectors) — and can give colonies the brain's MCP server. The token stays on the mothership as a saved secret; a colony
          only ever holds a placeholder that is swapped for it on the way to the MCP host.
        </p>
      }
    >
      {state === null ? (
        loadFailed ? (
          <p className="text-small-lg text-err">Living Brain's settings could not be loaded. Try opening the settings again.</p>
        ) : (
          <Spinner />
        )
      ) : (
        <div className="space-y-6">
          <ConnectionCard
            name="Living Brain"
            connected={state.state === "connected"}
            detail={livingBrainStatusText(state)}
            detailTone={state.state === "unauthorized" ? "err" : undefined}
            info={<p>Connect runs Living Brain's device sign-in from the mothership: you approve a short code in your browser, and the token never passes through this page.</p>}
          >
            {state.device && <DeviceCode device={state.device} onCancel={() => run(() => api.cancelLivingBrainConnect())} />}
            <div className="flex flex-wrap items-center gap-2">
              {state.state !== "connected" && !waiting && (
                <Button variant="primary" disabled={busy} onClick={connect}>
                  {busy && <Spinner />}
                  {state.state === "unauthorized" ? "Connect again" : "Connect Living Brain"}
                </Button>
              )}
              {connected && (
                <Button variant="secondary" disabled={busy} onClick={() => run(() => api.disconnectLivingBrain(), "Living Brain is disconnected; the token was revoked.")}>
                  Disconnect
                </Button>
              )}
            </div>
            {!connected && (
              <details className="text-small-lg">
                <summary className="cursor-pointer text-muted">Use a personal access token instead</summary>
                <div className="mt-2 flex flex-wrap gap-2">
                  <input
                    type="password"
                    autoComplete="off"
                    aria-label="Living Brain personal access token"
                    className={cx(inputClass, "flex-1")}
                    placeholder="lbp_…"
                    value={token}
                    onChange={(e) => setToken(e.target.value)}
                  />
                  <Button
                    disabled={busy || !token.trim()}
                    onClick={() => {
                      const t = token;
                      setToken("");
                      run(() => api.setLivingBrainToken(t), "Living Brain is connected.");
                    }}
                  >
                    Save token
                  </Button>
                </div>
                <p className="mt-1 text-muted">
                  Create one at{" "}
                  <a className="text-accent underline" href={`${state.server_url}/settings.html#tokens`} target="_blank" rel="noreferrer noopener">
                    Living Brain → Settings → API tokens
                  </a>
                  .
                </p>
              </details>
            )}
            <details className="text-small-lg">
              <summary className="cursor-pointer text-muted">Server</summary>
              <div className="mt-2 space-y-2">
                <label className="block">
                  <span className="text-muted">API and web app</span>
                  <input className={inputClass} value={server} disabled={connected || busy} onChange={(e) => setServer(e.target.value)} />
                </label>
                <label className="block">
                  <span className="text-muted">MCP server</span>
                  <input className={inputClass} value={mcp} disabled={connected || busy} onChange={(e) => setMcp(e.target.value)} />
                </label>
                {connected ? (
                  <p className="text-faint">Disconnect to change them: the token stays with the server that issued it.</p>
                ) : (
                  <Button variant="secondary" size="sm" disabled={busy || (server === state.server_url && mcp === state.mcp_url)} onClick={() => run(() => api.saveLivingBrain({ server_url: server, mcp_url: mcp }), "Saved.")}>
                    Save addresses
                  </Button>
                )}
              </div>
            </details>
          </ConnectionCard>

          <section className="space-y-2">
            <h4 className="text-small-lg font-semibold">Orgs and repositories</h4>
            <p className="text-small-lg text-muted">
              Everything is off until you add a rule. A repository's rule wins over its org's. A colony is sent once it is idle, waiting, parked or finished
              and its log has been quiet for half an hour — in the background, never in a colony's way.
            </p>
            {state.rules.length > 0 && (
              <ul className="divide-y divide-border">
                {state.rules.map((rule, i) => (
                  <RuleRow
                    key={rule.target}
                    rule={rule}
                    mcpAgents={state.mcp_agents}
                    busy={busy}
                    onChange={(next) => saveRules(state.rules.map((r, j) => (j === i ? next : r)))}
                    onRemove={() => saveRules(state.rules.filter((_, j) => j !== i))}
                  />
                ))}
              </ul>
            )}
            <div className="flex flex-wrap gap-2">
              <input
                className={cx(inputClass, "flex-1")}
                aria-label="Org or owner/repo"
                placeholder="org, or owner/repo"
                list="living-brain-orgs"
                value={newTarget}
                onChange={(e) => setNewTarget(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && addRule()}
              />
              <datalist id="living-brain-orgs">
                {(orgs ?? []).map((o) => (
                  <option key={o.org} value={o.org} />
                ))}
              </datalist>
              <Button variant="secondary" disabled={busy || !newTarget.trim()} onClick={addRule}>
                Add
              </Button>
            </div>
          </section>

          {connected && (
            <section className="space-y-2">
              <h4 className="text-small-lg font-semibold">Send queue</h4>
              <QueueStatus queue={state.queue} busy={busy} onRetry={() => api.retryLivingBrain().then(refresh).catch((error) => toast(errorMessage(error), "error"))} />
              <div className="space-y-2 rounded-xl border border-border px-3.5 py-3">
                <p className="text-small-lg text-muted">Send existing colonies — those that ended before a rule was switched on — for one org. Count first; nothing is sent until you confirm.</p>
                <div className="flex flex-wrap gap-2">
                  <select aria-label="Org to backfill" className={cx(inputClass, "w-auto")} value={backfillOrg} onChange={(e) => (setBackfillOrg(e.target.value), setCount(null))}>
                    <option value="">Choose an org</option>
                    {[...new Set([...(orgs ?? []).map((o) => o.org), ...state.rules.map((r) => r.target.split("/")[0])])].map((org) => (
                      <option key={org} value={org}>
                        {org}
                      </option>
                    ))}
                  </select>
                  <Button variant="secondary" disabled={busy || !backfillOrg} onClick={() => backfill(true)}>
                    Count
                  </Button>
                  {count && count.to_send > 0 && (
                    <Button disabled={busy} onClick={() => backfill(false)}>
                      Send {count.to_send} existing colonies for {count.org}
                    </Button>
                  )}
                </div>
                {count && <p className="text-small-lg text-muted">{backfillText(count)}</p>}
              </div>
            </section>
          )}
        </div>
      )}
    </Pane>
  );
}

/** The dry run, in words. */
export function backfillText(r: LivingBrainBackfill): string {
  const extra: string[] = [];
  if (r.already_sent > 0) extra.push(`${r.already_sent} already sent`);
  if (r.not_opted_in > 0) extra.push(`${r.not_opted_in} in repositories that do not send`);
  if (r.still_running > 0) extra.push(`${r.still_running} still running`);
  return `${r.to_send} of ${r.org}'s colonies would be sent${extra.length ? ` (${extra.join(", ")})` : ""}.`;
}
