import { useCallback, useEffect, useId, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { ProviderQuotaCard, QuotaChangeSummary, runQuotaAction } from "../../cockpit/ProviderQuotaCard";
import type { HarnessStatus, ModelOption, ModelProvider, PlanUsage, PriceFeedStatus, QuotaActionReply, QuotaCard } from "../../types";
import { Button, InfoButton, Spinner, cx, inputClass, timeAgo } from "../ui";
import { AddProvider } from "./AddProvider";
import { HealthStatus, type HealthView } from "./HealthStatus";
import { ClaudeListRow, ProviderListRow } from "./ProviderRows";
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
  claude,
  models,
  onOpenConnections,
  focusId,
  back,
}: {
  providers: ModelProvider[] | null;
  error: string | null;
  setProviders: (update: (list: ModelProvider[] | null) => ModelProvider[] | null) => void;
  reload: () => Promise<void>;
  claude: HarnessStatus["claude"] | null;
  models: ModelOption[];
  onOpenConnections: () => void;
  /** Opens this provider's editor when the pane appears — where a model picker's "Set key" lands. */
  focusId?: string;
  back?: () => void;
}) {
  const api = useApi();
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

  // The list is grouped rows; an open editor sits between two groups as a card of its own.
  const groups: ReactNode[] = [];
  let run: ReactNode[] = [];
  const flush = () => {
    if (run.length) groups.push(<div key={`g${groups.length}`} className={LIST}>{run}</div>);
    run = [];
  };
  run.push(
    <ClaudeListRow
      key="anthropic"
      claude={claude}
      models={models}
      plan={claudePlan}
      open={openId === "anthropic"}
      onToggle={() => toggle("anthropic")}
      onOpenConnections={onOpenConnections}
    />,
  );
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
      subtitle="Claude, and other Anthropic-compatible endpoints"
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
        {groups}
        {error && <p className="text-body-sm text-err">{error}</p>}
        {!providers && !error && (
          <p className="flex items-center gap-2 py-1 text-body-sm text-muted">
            <Spinner /> Loading providers…
          </p>
        )}
        {providers && providers.length === 0 && editing?.mode !== "new" && (
          <p className="rounded-xl border border-dashed border-border-strong px-3.5 py-4 text-center text-body-sm text-muted">No extra providers yet. Claude works without one.</p>
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
        <AddProvider disabled={!providers || editing !== null} onPick={(preset) => setEditing({ mode: "new", preset })} />
        <PriceFeedSection />
      </div>
    </Pane>
  );
}

/**
 * The price feed's URL setting (issue #1038): one input and save, with the feed's own status under it —
 * how many prices it carries, when it was last read, and what the last fetch failed with. Hidden until
 * the status arrives, so a mothership without /api/price-feed simply shows no section.
 */
function PriceFeedSection() {
  const api = useApi();
  const toast = useToast();
  const [status, setStatus] = useState<PriceFeedStatus | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const urlId = useId();
  useEffect(() => {
    let live = true;
    api
      .priceFeed()
      .then((s) => {
        if (!live) return;
        setStatus(s);
        setDraft(s.url ?? "");
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [api]);
  const save = async () => {
    setBusy(true);
    try {
      const saved = await api.savePriceFeed(draft);
      setStatus(saved);
      setDraft(saved.url ?? "");
      toast(saved.url ? "Price feed saved" : "Price feed turned off");
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(false);
    }
  };
  if (!status) return null;
  const changed = draft.trim() !== (status.url ?? "");
  return (
    <section className="space-y-2 rounded-xl border border-border px-3.5 py-3">
      <div className="flex items-center gap-1">
        <h3 className="text-body font-semibold">Price feed</h3>
        <InfoButton label="Price feed">
          <p>
            A JSON file of model prices the Mothership re-reads every so often, so priced routing works without typing
            every rate in. Your own rates win: a model's own price, then the connection's, then the feed's.
          </p>
        </InfoButton>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <input
          id={urlId}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="https://example.com/model-prices.json"
          spellCheck={false}
          autoComplete="off"
          aria-label="Price feed URL"
          className={cx(inputClass, "min-w-0 flex-1 font-mono text-body-sm")}
        />
        <Button size="sm" variant="primary" disabled={busy || !changed} onClick={save}>
          {busy && <Spinner />} Save
        </Button>
      </div>
      <p className="text-small text-muted">
        {status.url
          ? `${status.entries} price${status.entries === 1 ? "" : "s"} · ${status.fetched_at ? `fetched ${timeAgo(status.fetched_at)}` : "not fetched yet"}`
          : "Off — only the rates you set on a provider are counted."}
      </p>
      {status.last_error && <p className="text-small text-err">Last fetch failed: {status.last_error}</p>}
    </section>
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
