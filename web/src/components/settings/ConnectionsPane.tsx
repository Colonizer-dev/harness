import { GithubTokenForm } from "../Connections";
import { Avatar } from "../Avatar";
import type { HarnessStatus } from "../../types";
import { Code, ConnectionCard, Pane } from "./ui";

// ---------------------------------------------------------------------------
// Connections: GitHub. Every AI account lives under Models → Model providers (issue #1211).
// ---------------------------------------------------------------------------

export function ConnectionsPane({ status, onStatusChanged, back }: { status: HarnessStatus | null; onStatusChanged: (fresh?: boolean) => Promise<void> | void; back?: () => void }) {
  const github = status?.github ?? null;
  const githubViaToken = github?.source === "saved token";

  return (
    <Pane title="GitHub" subtitle="The code host colonies read from and push to" back={back}>
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
            <details className="text-body-sm">
              <summary className="cursor-pointer text-muted hover:text-text">{githubViaToken ? "Replace or remove the token" : "Use a token instead"}</summary>
              <div className="mt-2">
                <GithubTokenForm onStatusChanged={onStatusChanged} />
              </div>
            </details>
          ) : (
            <GithubTokenForm onStatusChanged={onStatusChanged} />
          )}
        </ConnectionCard>
      </div>
    </Pane>
  );
}
