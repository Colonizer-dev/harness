import { useCallback, useEffect, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { ProviderQuotaCard, QuotaChangeSummary, runQuotaAction } from "../../cockpit/ProviderQuotaCard";
import type { HarnessStatus, ModelOption, ModelProvider, PlanUsage, QuotaActionReply, QuotaCard } from "../../types";
import { Button, Spinner } from "../ui";
import { AddProvider } from "./AddProvider";
import { HealthStatus, type HealthView } from "./HealthStatus";
import { ProviderListRow } from "./ProviderRows";
import { AgentSubscriptionRow, ClaudeSubscriptionRow } from "./SubscriptionRows";
import { modelRoute } from "../../modelRoute";
import { openModelSwitcher } from "../../cockpit/ModelSwitcher";
import { ProviderForm } from "./ProviderForm";
import { Pane, Code } from "./ui";
import type { Editing } from "./providerCatalog";

export { HealthStatus };

const LIST = "overflow-hidden rounded-xl border border-border divide-y divide-border";

export function ProvidersPane({
  providers,
  error,
  setProviders,
  reload,
  status,
  models,
  onStatusChanged,
  focusId,
  back,
}: {
  providers: ModelProvider[] | null;
  error: string | null;
  setProviders: (update: (list: ModelProvider[] | null) => ModelProvider[] | null) => void;
  reload: () => Promise<void>;
  status: HarnessStatus | null;
  models: ModelOption[];
  onStatusChanged: (fresh?: boolean) => Promise<void> | void;
  /** Opens this provider's editor when the pane appears — where a model picker's "Set key" lands. */
  focusId?: string;
  back?: () => void;
}) {
  const api = useApi();
  const claude = status?.claude ?? null;
  const agents = (status?.agents ?? []).filter((a) => a.id !== "claude-code");
  const route = status ? modelRoute(status) : null;
  const [editing, setEditing] = useState<Editing>(() => (focusId ? { mode: "edit", id: focusId } : null));
  useEffect(() => {
    if (focusId) setEditing({ mode: "edit", id: focusId });
  }, [focusId]);
  const [health, setHealth] = useState<Record<string, HealthView>>({});
  // One row open at a time keeps the page a list, not a wall.
  const [openId, setOpenId] = useState<string | null>(null);
  const [claudePlan, setClaudePlan] = useState<PlanUsage | null>(null);

  // In-flight and queued counts change as colonies work; refresh them quietly.
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.hidden) return;
      void reload();
    }, 5000);
    return () => clearInterval(timer);
  }, [reload]);

  // The Claude subscription's limit state and reset come from the plans the model switcher reads.
  useEffect(() => {
    let live = true;
    const load = () =>
      api
        .modelPlans()
        .then((r) => live && setClaudePlan(r.plans.find((p) => p.kind === "claude") ?? null))
        .catch(() => undefined);
    void load();
    const timer = setInterval(() => !document.hidden && void load(), 30_000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [api]);

  const check = async (id: string) => {
    setHealth((h) => ({ ...h, [id]: { state: "checking" } }));
    try {
      const result = await api.providerHealth(id);
      setHealth((h) => ({ ...h, [id]: { state: "done", result } }));
      // The probe reads the plan balance; show the new reading on the row.
      void reload();
    } catch (e) {
      setHealth((h) => ({ ...h, [id]: { state: "failed", message: errorMessage(e) } }));
    }
  };

  const upsert = (saved: ModelProvider) =>
    setProviders((list) => {
      const rest = (list ?? []).filter((p) => p.id !== saved.id);
      const index = (list ?? []).findIndex((p) => p.id === saved.id);
      if (index < 0) return [...rest, saved];
      rest.splice(index, 0, saved);
      return rest;
    });

  const toggle = (id: string) => setOpenId((cur) => (cur === id ? null : id));

  // The subscriptions sit in a group of their own above the API providers.
  const subscriptions = (
    <div className={LIST}>
      <ClaudeSubscriptionRow
        claude={claude}
        models={models}
        plan={claudePlan}
        open={openId === "anthropic"}
        onToggle={() => toggle("anthropic")}
        onStatusChanged={onStatusChanged}
      />
      {agents.map((agent) => (
        <AgentSubscriptionRow
          key={agent.id}
          agent={agent}
          open={openId === `agent-${agent.id}`}
          isOrchestrator={status?.orchestrator?.module === agent.id}
          onToggle={() => toggle(`agent-${agent.id}`)}
          onAddKey={(preset) => setEditing({ mode: "new", preset })}
          onAddProvider={() => document.getElementById("add-provider")?.scrollIntoView({ block: "center" })}
        />
      ))}
    </div>
  );

  // The API providers are grouped rows; an open editor sits between two groups as a card of its own.
  const groups: ReactNode[] = [];
  let run: ReactNode[] = [];
  const flush = () => {
    if (run.length) groups.push(<div key={`g${groups.length}`} className={LIST}>{run}</div>);
    run = [];
  };
  for (const provider of providers ?? []) {
    if (editing?.mode === "edit" && editing.id === provider.id) {
      flush();
      groups.push(
        <ProviderForm
          key={provider.id}
          initial={provider}
          peers={providers ?? []}
          preset={provider.preset}
          takenIds={[]}
          onCancel={() => setEditing(null)}
          onSaved={(saved) => {
            upsert(saved);
            setHealth((h) => {
              const next = { ...h };
              delete next[saved.id];
              return next;
            });
            setEditing(null);
          }}
          onDeleted={(id) => {
            setProviders((list) => list?.filter((p) => p.id !== id) ?? null);
            setEditing(null);
          }}
        />,
      );
    } else {
      run.push(
        <ProviderListRow
          key={provider.id}
          provider={provider}
          health={health[provider.id]}
          disabled={editing !== null}
          open={openId === provider.id}
          onToggle={() => toggle(provider.id)}
          onCheck={() => void check(provider.id)}
          onEdit={() => setEditing({ mode: "edit", id: provider.id })}
        />,
      );
    }
  }
  flush();

  return (
    <Pane
      title="Model providers"
      subtitle="Every account and key your colonies can think with"
      back={back}
      info={
        <>
          <p>
            Claude is the default: a model picked by its plain id, such as <Code>sonnet</Code>, goes to Anthropic. Pick another provider's
            model as <Code>provider/model</Code>, for example <Code>deepseek/deepseek-flash</Code>, wherever you choose an orchestrator,
            subagent or background model.
          </p>
          <p className="text-muted">
            Requests go through the Mothership gateway: colonies never see provider keys, and servers on a private network (LAN, tailnet)
            work without extra setup.
          </p>
        </>
      }
    >
      <div className="space-y-3">
        <ProviderQuotaCards reloadProviders={reload} />
        {route && <OrchestratorCard route={route} model={status?.orchestrator?.model ?? ""} agent={status?.orchestrator?.module ?? null} />}
        <GroupHeading id="subscriptions-heading" title="Subscriptions" help="Sign in once; colonies use the account." />
        {subscriptions}
        <GroupHeading id="api-providers-heading" title="API providers" help="An endpoint and a key, or a server on your network." />
        {groups}
        {error && <p className="text-body-sm text-err">{error}</p>}
        {!providers && !error && (
          <p className="flex items-center gap-2 py-1 text-body-sm text-muted">
            <Spinner /> Loading providers…
          </p>
        )}
        {providers && providers.length === 0 && editing?.mode !== "new" && (
          <p className="rounded-xl border border-dashed border-border-strong px-3.5 py-4 text-center text-body-sm text-muted">No API providers yet. A Claude login works without one.</p>
        )}
        {providers && editing?.mode === "new" && (
          <ProviderForm
            key={`new-${editing.preset}`}
            preset={editing.preset}
            peers={providers}
            takenIds={providers.map((p) => p.id)}
            onCancel={() => setEditing(null)}
            onSaved={(saved) => {
              upsert(saved);
              setOpenId(saved.id);
              setEditing(null);
            }}
          />
        )}
        <div id="add-provider">
          <AddProvider disabled={!providers || editing !== null} onPick={(preset) => setEditing({ mode: "new", preset })} />
        </div>
      </div>
    </Pane>
  );
}

/**
 * The "Provider out of quota" cards (issue #767) at the top of the providers pane: the same cards
 * the inbox shows, answered here the same way. Refreshed on the pane's own 5 s rhythm.
 */
function ProviderQuotaCards({ reloadProviders }: { reloadProviders: () => Promise<void> }) {
  const api = useApi();
  const toast = useToast();
  const [cards, setCards] = useState<QuotaCard[]>([]);
  // The last switch's "was X → now Y" summary, kept after its card goes (issue #767).
  const [switched, setSwitched] = useState<QuotaActionReply | null>(null);
  const load = useCallback(async () => {
    try {
      setCards((await api.attention()).quota_cards ?? []);
    } catch {
      // An older mothership has no /api/attention: no cards, nothing to say.
    }
  }, [api]);
  useEffect(() => {
    void load();
    const timer = setInterval(() => {
      if (!document.hidden) void load();
    }, 5000);
    return () => clearInterval(timer);
  }, [load]);
  if (cards.length === 0 && !switched) return null;
  return (
    <div className="space-y-2">
      <QuotaChangeSummary reply={switched} onDismiss={() => setSwitched(null)} />
      {cards.map((card) => (
        <ProviderQuotaCard
          key={card.provider}
          card={card}
          onAction={async (provider, body) => {
            const reply = await runQuotaAction(api.quotaAction, (message, tone) => toast(message, tone), provider, body);
            if ((reply?.changes?.length ?? 0) > 0) setSwitched(reply);
            await Promise.all([load(), reloadProviders()]);
          }}
        />
      ))}
    </div>
  );
}

function GroupHeading({ id, title, help }: { id: string; title: string; help: string }) {
  return (
    <div className="pt-1">
      <h3 id={id} className="text-meta-lg font-medium uppercase tracking-wide text-muted">
        {title}
      </h3>
      <p className="mt-0.5 text-small leading-snug text-faint">{help}</p>
    </div>
  );
}

/** Which model the orchestrator runs on and whether a colony can reach it: the Models card of issue #1211. */
function OrchestratorCard({ route, model, agent }: { route: ReturnType<typeof modelRoute>; model: string; agent: string | null }) {
  return (
    <div data-orchestrator-route={route.ok ? "ok" : "broken"} className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border border-border px-4 py-3">
      <span aria-hidden="true" className={`size-2 shrink-0 rounded-full ${route.ok ? "bg-ok" : "bg-err"}`} />
      <div className="min-w-0 flex-1">
        <p className="text-body-sm font-medium">{route.ok ? "Colonies can reach a model" : "Colonies have no model to use"}</p>
        <p className="text-small text-muted [overflow-wrap:anywhere]">
          {route.ok
            ? `The orchestrator${model ? ` (${model})` : ""} runs through ${route.detail}.`
            : (route.reason ?? "No route to a model.")}
          {agent ? ` Agent: ${agent}.` : ""}
        </p>
      </div>
      <Button size="sm" onClick={openModelSwitcher}>
        Switch the orchestrator model
      </Button>
    </div>
  );
}
