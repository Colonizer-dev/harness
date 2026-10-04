import { ClaudeLoginSection, GithubTokenForm } from "../Connections";
import { Avatar } from "../Avatar";
import { Badge, timeAgo } from "../ui";
import type { HarnessStatus } from "../../types";
import { Code, ConnectionCard, Pane } from "./ui";

// ---------------------------------------------------------------------------
// The Claude card's health badge: the background per-account check's verdict (issue
// #983), with when it last ran. No badge for an older mothership that sends none.
// ---------------------------------------------------------------------------

const CLAUDE_HEALTH: Record<NonNullable<HarnessStatus["claude"]["health_status"]>, { tone: "ok" | "err" | "warn" | "neutral"; label: string }> = {
  ok: { tone: "ok", label: "Reachable" },
  auth_expired: { tone: "err", label: "Token rejected" },
  unreachable: { tone: "warn", label: "Unreachable" },
  unchecked: { tone: "neutral", label: "Not checked yet" },
};

function ClaudeHealth({ status, checkedAt }: { status: HarnessStatus["claude"]["health_status"]; checkedAt: string | null | undefined }) {
  if (!status) return null;
  const health = CLAUDE_HEALTH[status];
  return (
    <p className="flex flex-wrap items-center gap-2 text-[12.5px] text-muted">
      <Badge tone={health.tone}>{health.label}</Badge>
      {checkedAt ? <span>checked {timeAgo(checkedAt)}</span> : null}
    </p>
  );
}

// ---------------------------------------------------------------------------
// Connections: GitHub and Claude
// ---------------------------------------------------------------------------

export function ConnectionsPane({ status, onStatusChanged, back }: { status: HarnessStatus | null; onStatusChanged: (fresh?: boolean) => Promise<void> | void; back?: () => void }) {
  const github = status?.github ?? null;
  const claude = status?.claude ?? null;
  const githubViaToken = github?.source === "saved token";

  return (
    <Pane title="Connections" subtitle="Both are needed before the first colony" back={back}>
      <div className="space-y-4">
        <ConnectionCard
          name="GitHub"
          mark={github?.connected && github.avatar_url ? <Avatar name={github.login || "GitHub"} src={github.avatar_url} /> : undefined}
          connected={github ? github.connected : null}
          detail={github?.connected ? `@${github.login} · ${github.source}` : github?.error?.split("\n")[0]}
          detailTone={github && !github.connected ? "err" : undefined}
          info={
            <>
              <p>
                Uses your <Code>gh auth login</Code> session when there is one, or a token you save here.
              </p>
              <p className="text-muted">A fine-grained token can start read-only (Contents, Issues and Pull requests, read). Add write to Contents and Pull requests once you're ready to publish.</p>
            </>
          }
        >
          {github?.connected ? (
            <details className="text-[13px]">
              <summary className="cursor-pointer text-muted hover:text-text">{githubViaToken ? "Replace or remove the token" : "Use a token instead"}</summary>
              <div className="mt-2">
                <GithubTokenForm onStatusChanged={onStatusChanged} />
              </div>
            </details>
          ) : (
            <GithubTokenForm onStatusChanged={onStatusChanged} />
          )}
        </ConnectionCard>

        <ConnectionCard
          name="Claude"
          connected={claude ? claude.configured : null}
          detail={claude?.configured ? [claude.account ?? "account not identified", claude.source].filter(Boolean).join(" · ") : undefined}
          info={
            <>
              <p>
                Log in runs <Code>claude setup-token</Code> on the Mothership (this machine) and saves a 1-year token here.
              </p>
              <p className="text-muted">microVMs only ever see a placeholder; the real token is swapped in for requests to api.anthropic.com.</p>
              <p className="text-muted">
                Anthropic does not report an expiry, so the harness shows the documented 1-year lifetime as an estimate.
              </p>
            </>
          }
        >
          {claude?.configured && <ClaudeHealth status={claude.health_status} checkedAt={claude.health_checked_at} />}
          <ClaudeLoginSection claude={claude} onStatusChanged={onStatusChanged} />
        </ConnectionCard>
      </div>
    </Pane>
  );
}
