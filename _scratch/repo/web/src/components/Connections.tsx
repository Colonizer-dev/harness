// The GitHub token form and the Claude login flows, in one implementation shared by the
// Connections pane and the Setup checklist (issue #129). Both call `onStatusChanged` when
// something they did should re-read GET /api/status; `useApi` and `useToast` are app-global,
// so these render anywhere.
import { useEffect, useId, useState, type FormEvent } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { HarnessStatus, LoginView } from "../types";
import { Badge, Button, Spinner, cx, inputClass, timeAgo } from "./ui";
import { IconCheck, IconExternal } from "./icons";

const IDLE_LOGIN: LoginView = { state: "idle", url: null, message: null };

/**
 * The saved Claude credential's provenance, under the login buttons: why the account
 * may not be identified, and when the token dies. Anthropic does not report an expiry,
 * so an estimated one says "about"; past, or within 30 days, it turns into a warning.
 */
function ClaudeCredential({ claude }: { claude: HarnessStatus["claude"] }) {
  const asDate = (ts: string | null | undefined) => {
    if (!ts) return null;
    const date = new Date(ts);
    return isNaN(date.getTime()) ? null : date;
  };
  const savedAt = asDate(claude.saved_at);
  const expiresAt = asDate(claude.expires_at);
  const msLeft = expiresAt ? expiresAt.getTime() - Date.now() : null;
  const expired = msLeft != null && msLeft <= 0;
  const daysLeft = msLeft != null && msLeft > 0 ? msLeft / 86_400_000 : null;
  const expiringSoon = daysLeft != null && daysLeft <= 30;
  return (
    <div className="space-y-1 text-[12.5px] [overflow-wrap:anywhere]">
      {claude.account_note && <p className="text-muted">{claude.account_note}</p>}
      {(savedAt || expiresAt) && (
        <p className="text-muted">
          {savedAt && <span>Saved {savedAt.toLocaleDateString()}</span>}
          {savedAt && expiresAt && " · "}
          {expiresAt &&
            (expired ? (
              <span className="text-err">
                {claude.expires_estimated ? "estimated expiry passed " : "expired "}
                {timeAgo(claude.expires_at)}
              </span>
            ) : (
              <span className={cx(expiringSoon && "text-warn")}>
                expires {claude.expires_estimated ? "about " : ""}
                {expiresAt.toLocaleDateString()}
              </span>
            ))}
        </p>
      )}
      {expiringSoon && daysLeft != null && (
        <div>
          <Badge tone="warn">
            Expires in {Math.ceil(daysLeft)} day{Math.ceil(daysLeft) === 1 ? "" : "s"}
          </Badge>
        </div>
      )}
    </div>
  );
}

/** Paste a GitHub token, or remove a saved one. The alternative to `gh auth login` on the host. */
export function GithubTokenForm({ onStatusChanged }: { onStatusChanged: (fresh?: boolean) => Promise<void> | void }) {
  const api = useApi();
  const toast = useToast();
  const githubTokenId = useId();
  const [token, setToken] = useState("");
  const [saving, setSaving] = useState(false);

  const run = async (fn: () => Promise<unknown>, success: string) => {
    setSaving(true);
    try {
      await fn();
      toast(success);
      await onStatusChanged();
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className="flex flex-wrap gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        void run(() => api.setGithubToken(token.trim()), "GitHub token saved").then(() => setToken(""));
      }}
    >
      <label htmlFor={githubTokenId} className="sr-only">
        GitHub token
      </label>
      <input
        id={githubTokenId}
        type="password"
        autoComplete="off"
        value={token}
        onChange={(e) => setToken(e.target.value)}
        placeholder="github_pat_… or ghp_…"
        className={cx(inputClass, "min-w-48 flex-1")}
      />
      <Button type="submit" variant="primary" disabled={!token.trim() || saving}>
        {saving && <Spinner />} Save
      </Button>
      <Button disabled={saving} onClick={() => run(() => api.deleteGithubToken(), "Saved GitHub token removed")}>
        Remove saved token
      </Button>
    </form>
  );
}

/** The whole Claude side of a connection: subscription login (with its poll for the code),
 *  the saved credential's summary, and a token or API key as the way around a missing host binary. */
export function ClaudeLoginSection({
  claude,
  onStatusChanged,
}: {
  claude: HarnessStatus["claude"] | null;
  onStatusChanged: (fresh?: boolean) => Promise<void> | void;
}) {
  const api = useApi();
  const toast = useToast();
  const claudeTokenId = useId();
  const codeId = useId();
  const [claudeToken, setClaudeToken] = useState("");
  const [saving, setSaving] = useState(false);
  const [login, setLogin] = useState<LoginView>(IDLE_LOGIN);
  const [code, setCode] = useState("");
  const flowActive = login.state === "starting" || login.state === "awaiting_code" || login.state === "verifying";

  useEffect(() => {
    if (!flowActive) return;
    const timer = setInterval(async () => {
      try {
        const view = await api.claudeLogin();
        setLogin(view);
        if (view.state === "done") {
          toast(view.message || "Claude subscription connected");
          void onStatusChanged();
        }
      } catch {
        /* retry on next tick */
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [api, flowActive, onStatusChanged, toast]);

  const runSaved = async (fn: () => Promise<unknown>, success: string) => {
    setSaving(true);
    try {
      await fn();
      toast(success);
      await onStatusChanged();
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setSaving(false);
    }
  };

  const startLogin = async () => {
    setCode("");
    try {
      setLogin(await api.claudeLoginStart());
    } catch (error) {
      setLogin({ state: "error", url: null, message: errorMessage(error) });
    }
  };

  const submitCode = async (event: FormEvent) => {
    event.preventDefault();
    try {
      setLogin(await api.claudeLoginCode(code.trim()));
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  };

  return (
    <>
      <div className="flex flex-wrap gap-2">
        <Button variant="primary" onClick={startLogin} disabled={flowActive}>
          {login.state === "starting" && <Spinner />} Log in with Claude subscription
        </Button>
        <Button onClick={() => runSaved(() => api.deleteClaudeToken(), "Saved Claude token removed")} disabled={saving}>
          Remove saved token
        </Button>
      </div>

      {claude && claude.configured && <ClaudeCredential claude={claude} />}

      {login.state !== "idle" && (
        <div role="status" className="space-y-3 rounded-lg border border-dashed border-border-strong p-3.5">
          {login.state === "starting" && (
            <p className="flex items-center gap-2 text-[13px] text-muted">
              <Spinner /> Starting claude setup-token…
            </p>
          )}
          {(login.state === "awaiting_code" || login.state === "verifying") && (
            <>
              <ol className="space-y-1.5 text-[13px]">
                <li>
                  <span className="mr-1 font-semibold">1.</span>
                  {login.url?.startsWith("https://") ? (
                    <a
                      href={login.url}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="inline-flex items-center gap-1 font-medium text-accent hover:underline"
                    >
                      Open the Claude sign-in page <IconExternal size={12} />
                    </a>
                  ) : (
                    "Waiting for the sign-in link…"
                  )}{" "}
                  and approve access.
                </li>
                <li>
                  <span className="mr-1 font-semibold">2.</span>Paste the code it shows.
                </li>
              </ol>
              <form onSubmit={submitCode} className="flex flex-wrap gap-2">
                <label htmlFor={codeId} className="sr-only">
                  Sign-in code
                </label>
                <input
                  id={codeId}
                  value={code}
                  onChange={(e) => setCode(e.target.value)}
                  autoComplete="off"
                  placeholder="Sign-in code"
                  className={cx(inputClass, "min-w-48 flex-1 font-mono")}
                />
                <Button type="submit" variant="primary" disabled={!code.trim() || login.state === "verifying"}>
                  {login.state === "verifying" && <Spinner />} Submit
                </Button>
                <Button onClick={async () => setLogin(await api.claudeLoginCancel().catch(() => IDLE_LOGIN))}>Cancel</Button>
              </form>
            </>
          )}
          {login.state === "done" && (
            <p className="flex items-center gap-2 text-[13px] text-ok">
              <IconCheck size={14} /> {login.message ?? "Connected"}
            </p>
          )}
          {login.state === "error" && <p className="text-[13px] text-err">{login.message ?? "Sign-in failed"}</p>}
        </div>
      )}

      <details className="text-[13px]">
        <summary className="cursor-pointer text-muted hover:text-text">Use a token or API key instead</summary>
        <form
          className="mt-2 flex flex-wrap gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void runSaved(() => api.setClaudeToken(claudeToken.trim()), "Claude token saved").then(() => setClaudeToken(""));
          }}
        >
          <label htmlFor={claudeTokenId} className="sr-only">
            Claude token
          </label>
          <input
            id={claudeTokenId}
            type="password"
            autoComplete="off"
            value={claudeToken}
            onChange={(e) => setClaudeToken(e.target.value)}
            placeholder="sk-ant-oat01-… or sk-ant-api…"
            className={cx(inputClass, "min-w-48 flex-1")}
          />
          <Button type="submit" disabled={!claudeToken.trim() || saving}>
            Save
          </Button>
        </form>
      </details>
    </>
  );
}
