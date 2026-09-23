// The one place cost is added up and formatted (issue #209). Every "spent" figure — the overview
// header, the workspace header chip, the org cards, a colony's sidebar row — reads through here, so
// "unmeasured" can never be rendered as $0.00: null means no report ever measured a cost, and it is
// shown as an em dash, not as a decimal. A measured zero ($0.00) stays a measured zero.

/** The two independent cost reports a colony or org carries: the Claude-side `cost_usd` and the
 *  gateway's `routed_cost_usd`. Both are null/absent when nothing has been measured (a subscription
 *  colony). A `Session` and an `OrgSpend` are both assignable to this. */
export interface CostPair {
  cost_usd?: number | null;
  routed_cost_usd?: number | null;
}

/** A colony's total spend, null only when neither report has ever measured a cost. */
export function sessionCost(session: CostPair): number | null {
  return sumCosts([session.cost_usd ?? null, session.routed_cost_usd ?? null]);
}

/** An org's total spend (GET /api/orgs `spend`), same "measured or not" rule as `sessionCost`. */
export function orgCost(spend: { cost_usd: number | null; routed_cost_usd: number | null } | undefined | null): number | null {
  if (!spend) return null;
  return sumCosts([spend.cost_usd, spend.routed_cost_usd]);
}

/** Sum a list of costs, treating an unmeasured (null) member as $0; null only when every member is null. */
export function sumCosts(values: (number | null)[]): number | null {
  let measured = false;
  let total = 0;
  for (const value of values) {
    if (value == null) continue;
    measured = true;
    total += value;
  }
  return measured ? total : null;
}

/** Render a cost: "—" when unmeasured, else "$X.XX". A measured zero IS "$0.00". */
export function formatCost(value: number | null): string {
  return value == null ? "—" : `$${value.toFixed(2)}`;
}

/** A compact token count: 999 → "999", 1000 → "1k", 1200 → "1.2k", 1.2M, 3.4B (one decimal, tail .0
 *  trimmed). A prefix that rounds up to a full 1000 is the next unit up, never "1000k": 999_999 is
 *  "1M". */
export function formatTokens(n: number): string {
  const abs = Math.abs(n);
  if (abs < 1000) return String(n);
  const units: [number, string][] = [
    [1e9, "B"],
    [1e6, "M"],
    [1e3, "k"],
  ];
  const compact = (value: number, unit: string) => {
    const rounded = Math.round(value * 10) / 10;
    const text = Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
    return `${text}${unit}`;
  };
  for (const [index, [factor, unit]] of units.entries()) {
    if (abs >= factor) {
      const value = n / factor;
      const rounded = Math.round(value * 10) / 10;
      // Rounding the last tenth can grow a prefix to exactly 1000 — 999_999 reads as 999.999k.
      // The next unit up is the truth. At the top unit there is nothing to roll into, so 1000B stays.
      return Math.abs(rounded) >= 1000 && index > 0
        ? compact(rounded / 1000, units[index - 1][1])
        : compact(rounded, unit);
    }
  }
  return String(n);
}

/** The top `max` models by tokens, plus how many are left out ("and N more"). */
export function modelMix(
  models: { model: string; tokens: number }[] | undefined | null,
  max = 3,
): { shown: { model: string; tokens: number }[]; more: number } {
  const ordered = [...(models ?? [])].sort((a, b) => b.tokens - a.tokens);
  return { shown: ordered.slice(0, max), more: Math.max(0, ordered.length - max) };
}