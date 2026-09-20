// The spend block on an org card in the overview (issue #209): what the org has spent in total,
// its token tally, its top models, and a 30-day sparkline of daily spend. Renders nothing for an
// older mothership that carries neither an org rollup nor history, so the card falls back to the
// plain row. All "unmeasured" costs come from ../spend, so they show "—" and never "$0.00".
import type { ReactElement } from "react";

import { formatCost, formatTokens, modelMix, orgCost } from "../spend";
import type { OrgSpend as OrgSpendData, SpendOrgDay } from "../types";

const SPARKLINE_WIDTH = 116;
const SPARKLINE_HEIGHT = 26;

/** One bar per returned day, paired with its "YYYY-MM-DD"; a day this org stayed out of is `org: undefined`. */
export interface OrgSpendHistoryDay {
  day: string;
  org: SpendOrgDay | undefined;
}

/** One bar per returned day; a day this org stayed out of lands as a zero-height slot. */
function SpendSparkline({ days }: { days: OrgSpendHistoryDay[] }): ReactElement {
  const costs = days.map((entry) =>
    entry.org ? orgCost({ cost_usd: entry.org.cost_usd, routed_cost_usd: entry.org.routed_cost_usd }) : null,
  );
  // All days unmeasured → no bars: a row of zeroes would read as "$0.00 every day", which is a
  // statement the API never made. An em dash says "never measured" instead.
  if (costs.every((cost) => cost == null)) {
    return (
      <span
        role="img"
        aria-label="Spend, last 30 days"
        title="no measured spend in the last 30 days"
        className="grid h-[26px] w-[116px] shrink-0 place-items-center font-mono text-xs text-faint"
      >
        —
      </span>
    );
  }
  const max = Math.max(...costs.map((cost) => cost ?? 0));
  const slot = SPARKLINE_WIDTH / days.length;
  const barWidth = Math.max(2, Math.min(4, slot - 2));
  return (
    <svg
      role="img"
      aria-label="Spend, last 30 days"
      width={SPARKLINE_WIDTH}
      height={SPARKLINE_HEIGHT}
      className="shrink-0"
    >
      {days.map((entry, i) => {
        const cost = costs[i];
        // A null day is unmeasured, not $0, so it gets no bar and no height; a tiny but measured day
        // keeps at least a pixel so it is visible next to a big-ticket day.
        const height = cost == null ? 0 : cost > 0 ? Math.max(1, Math.round((cost / max) * SPARKLINE_HEIGHT)) : 0;
        return (
          <rect
            key={entry.day}
            x={Math.round(i * slot) + 1}
            y={SPARKLINE_HEIGHT - height}
            width={barWidth}
            height={height}
            fill="var(--accent)"
            rx={1}
          >
            <title>{`${entry.day}: ${formatCost(cost)}`}</title>
          </rect>
        );
      })}
    </svg>
  );
}

export function OrgSpend({
  spend,
  history,
}: {
  /** The org's rollup from GET /api/orgs; absent on a mothership that does not measure org spend. */
  spend: OrgSpendData | undefined;
  /** One slot per day from GET /api/spend/history (ascending); a day the org was away is `org: undefined`. */
  history?: OrgSpendHistoryDay[];
}): ReactElement | null {
  if (spend === undefined && (history === undefined || history.length === 0)) return null;

  const cost = orgCost(spend);
  const tokens = spend ? spend.tokens.input + spend.tokens.output + spend.tokens.cache_read + spend.tokens.cache_write : null;
  const mix = modelMix(spend?.models, 3);

  return (
    <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-2 border-b border-border px-3.5 py-3">
      <div className="flex min-w-0 flex-col gap-1">
        <span className="flex items-baseline gap-2 font-mono tabular-nums">
          <span className="text-[13px] font-semibold text-text" title="what this org's colonies have spent in total">
            {formatCost(cost)}
          </span>
          {tokens != null && <span className="text-[10.5px] text-faint">{formatTokens(tokens)} tokens</span>}
        </span>
        {mix.shown.length > 0 && (
          <span className="flex flex-wrap gap-x-2 gap-y-0.5 font-mono text-[10.5px] text-faint">
            {mix.shown.map((model) => (
              <span key={model.model} className="whitespace-nowrap">
                <span title={`${formatTokens(model.tokens)} tokens`}>{model.model}</span>
                <span className="ml-1 text-muted">{formatTokens(model.tokens)}</span>
              </span>
            ))}
            {mix.more > 0 && <span className="whitespace-nowrap text-muted">· and {mix.more} more</span>}
          </span>
        )}
      </div>
      {history ? <SpendSparkline days={history} /> : null}
    </div>
  );
}