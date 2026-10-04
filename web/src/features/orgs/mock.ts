// The `orgs` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { clone, isLive, sleep } from "../../mockShared";
import { REPOS } from "../../features/repos/mock";
import type { ModelSpend, OrgInfo, OrgSpend, SpendDay, SpendHistory, SpendOrgDay, SpendTokens } from "../../types";
import type { MockState } from "../../mockState";
import type { OrgsApi } from "./api";

export const isoDay = (offsetFromToday: number): string => {
  const d = new Date(Date.now() - offsetFromToday * 86_400_000);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

// Per-org spend (issue #209), served both by GET /api/orgs `spend` and rolled up by
// GET /api/spend/history. octocat is the subscription case: its colonies run on unpriced models, so
// the costs are all null and the org card and sparkline must render "—", never "$0.00".
export const MOCK_SPEND_MODELS: ModelSpend[] = [
  { model: "claude-opus-5", tokens: 4_632_000, cost_usd: 41.28 },
  { model: "deepseek/deepseek-flash", tokens: 1_204_000, cost_usd: 2.91 },
  { model: "strix/ds4-flash", tokens: 810_400, cost_usd: null },
  { model: "claude-haiku-4-5", tokens: 212_000, cost_usd: 0.18 },
];
export const MOCK_SPEND_TOKENS: SpendTokens = { input: 4_320_000, output: 1_140_000, cache_read: 3_200_000, cache_write: 180_000 };

/**
 * Splits one history day's measured cost/tokens across the cumulative model mix, so the
 * spend-by-model stacks agree with the day total instead of repeating the rollup. Tokens
 * follow the mix weights (largest remainder, so they add up exactly); cost follows the priced
 * weights in cents with the rounding drift on the largest share, and the unpriced model keeps
 * cost null, like the rollup.
 */
export function splitDayModels(cost: number, input: number, output: number): ModelSpend[] {
  const dayTotal = input + output;
  const tokenTotal = MOCK_SPEND_MODELS.reduce((n, m) => n + m.tokens, 0);
  const shares = MOCK_SPEND_MODELS.map((m) => (dayTotal * m.tokens) / tokenTotal);
  const tokens = shares.map(Math.floor);
  let rest = dayTotal - tokens.reduce((n, v) => n + v, 0);
  const order = shares.map((_, i) => i).sort((a, b) => shares[b] - tokens[b] - (shares[a] - tokens[a]));
  for (let k = 0; k < rest; k++) tokens[order[k % order.length]] += 1;
  const cents = Math.round(cost * 100);
  const costTotal = MOCK_SPEND_MODELS.reduce((n, m) => n + (m.cost_usd ?? 0), 0);
  const costs: (number | null)[] = MOCK_SPEND_MODELS.map((m) => (m.cost_usd == null || costTotal <= 0 ? null : Math.round((cents * m.cost_usd) / costTotal) / 100));
  const drift = cents - costs.reduce<number>((n, c) => n + Math.round((c ?? 0) * 100), 0);
  const biggest = costs.indexOf(Math.max(...costs.map((c) => c ?? -1)));
  if (biggest >= 0 && costs[biggest] != null) costs[biggest] = (costs[biggest] as number) + drift / 100;
  return MOCK_SPEND_MODELS.map((m, i) => ({ model: m.model, tokens: tokens[i], cost_usd: costs[i] }));
}
export const mockOrgSpend: Record<string, OrgSpend> = {
  acme: { cost_usd: 44.37, routed_cost_usd: 0, tokens: MOCK_SPEND_TOKENS, models: MOCK_SPEND_MODELS },
  octocat: {
    cost_usd: null,
    routed_cost_usd: null,
    tokens: { input: 88_000, output: 14_000, cache_read: 0, cache_write: 0 },
    models: [{ model: "claude-sonnet-5", tokens: 102_000, cost_usd: null }],
  },
};

export function orgsMock(ms: MockState): OrgsApi {
  return {
    orgs: () =>
      ms.later((): OrgInfo[] => {
    const names = new Set([
      ...REPOS.map((r) => r.full_name.split("/")[0]),
      ...[...ms.sessions.values()].map((s) => s.session.repo.split("/")[0]),
      ...Object.keys(ms.orgSettings),
    ]);
    return [...names].sort().map((org) => {
      const colonies = [...ms.sessions.values()].filter((s) => s.session.repo.split("/")[0] === org);
      return {
        org,
        colonies: { live: colonies.filter((s) => isLive(s.session.status)).length, total: colonies.length },
        pending_memory: ms.proposals.filter((p) => ms.orgOfKey(p) === org).length,
        settings: ms.orgSettings[org] ?? {},
        avatar_url: ms.orgAvatars[org],
        ...(ms.awaitingDecision.has(org.toLowerCase()) ? { awaiting_decision: true } : null),
        ...(mockOrgSpend[org] ? { spend: mockOrgSpend[org] } : null),
      };
    });
      }),
    activity: (q = {}) =>
      ms.later(() => {
        const limit = q.limit ?? 100;
        const needle = q.q?.trim().toLowerCase() ?? "";
        const matching = [...ms.activityLog]
          .reverse()
          .filter((e) => q.before == null || e.seq < q.before)
          .filter((e) => !q.org || !e.org || e.org.toLowerCase() === q.org.toLowerCase())
          .filter((e) => !q.repo || e.repo === q.repo)
          .filter((e) => !q.actor || e.actor === q.actor)
          .filter((e) => !q.kind || q.kind.split(",").some((k) => e.kind === k || e.kind.startsWith(`${k}.`)))
          .filter((e) => !needle || [e.repo, e.title, e.target, e.detail, e.colony].some((f) => f?.toLowerCase().includes(needle)));
        const entries = matching.slice(0, limit);
        return { entries, next_before: matching.length > limit ? entries[entries.length - 1].seq : null, skipped: 0 };
      }),
    spendHistory: (days = 8) =>
      ms.later((): SpendHistory => {
    // Deterministic days, oldest first. acme is measured and roars some days; octocat is
    // measured-but-never-priced so its costs stay null. Every fourth day only octocat appears,
    // so acme's sparkline has zero-height (gap) slots. The default eight days keep the
    // long-standing shape; an explicit window (the overview asks for twice its range) extends
    // the same pattern further back.
    const dayCount = Math.max(0, Math.floor(days));
    const acmeCosts = [0.35, 1.1, 0.8, 2.3, 0.6, 1.7, 0.4, 0.9];
    const result: SpendDay[] = Array.from({ length: dayCount }, (_, i) => {
      const orgs: SpendOrgDay[] = [];
      if (i % 4 !== 1) {
        const cost = acmeCosts[i % acmeCosts.length];
        const input = 40_000 * (cost + 1);
        const output = 8_000 * (cost + 1);
        orgs.push({
          org: "acme",
          cost_usd: cost,
          routed_cost_usd: 0,
          tokens: { input, output, cache_read: 0, cache_write: 0 },
          models: splitDayModels(cost, input, output),
          launched: i % 2 === 0 ? 1 : 0,
          returned: i % 3 === 0 ? 1 : 0,
        });
      }
      if (i % 2 === 1) {
        orgs.push({
          org: "octocat",
          cost_usd: null,
          routed_cost_usd: null,
          tokens: { input: 12_000, output: 2_000, cache_read: 0, cache_write: 0 },
          models: [{ model: "claude-sonnet-5", tokens: 14_000, cost_usd: null }],
          launched: 0,
          returned: 1,
        });
      }
      return { day: isoDay(dayCount - 1 - i), orgs };
    });
    return { days: result };
      }),
    saveOrg: async (org, settings) => {
      await sleep(250);
      // The server merges: a field the body omits keeps its saved value, one it names (null
      // included) wins. The prompt card answers with `{enabled}` alone, so a replace here would
      // clear every other setting the org has — the merge is the semantics. The merged settings are
      // what comes back, as from the server.
      ms.orgSettings[org] = { ...(ms.orgSettings[org] ?? {}), ...settings };
      ms.awaitingDecision.delete(org.toLowerCase());
      return clone({ org, settings: ms.orgSettings[org] });
    }
  };
}
