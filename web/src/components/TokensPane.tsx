// Settings → API tokens (issue #646): the scoped tokens of api_tokens.rs, minted and revoked here
// instead of through `colonizer token`. The registry keeps only a hash of each token, so a create
// answer is the one time the plaintext exists anywhere the cockpit could show it — hence the
// one-time reveal below. The routes are the owner's alone (docs/cli.md, "Scoped API tokens"):
// managing a credential is not a thing a credential may do.
import { cloneElement, useEffect, useState, type FormEvent, type ReactElement, type ReactNode } from "react";

import { errorMessage, useApi, useToast } from "../context";
import type { ApiTokenMeta, ApiTokenScope, CreatedApiToken, NewApiToken } from "../types";
import { Pane } from "./SettingsDialog";
import { Badge, Button, Spinner, cx, inputClass, timeAgo, type Tone } from "./ui";

/** The scopes a new token may take, in the order the registry ranks them, with the one-line hint the picker shows.
 * `fleet` is absent on purpose: it is minted by fleet pairing, never created by hand (docs/fleet.md). */
export const TOKEN_SCOPES: readonly { id: ApiTokenScope; hint: string }[] = [
  { id: "read", hint: "Watch the mothership and its colonies — nothing is driven." },
  { id: "operate", hint: "Also drive colonies that exist: answer, stop, resume." },
  { id: "launch", hint: "Also start colonies, and create, edit and run its own loops." },
];

/** The create form's raw fields; the list-shaped ones hold whatever the user typed. */
export interface TokenForm {
  name: string;
  scope: ApiTokenScope;
  orgs: string;
  repos: string;
  maxConcurrent: string;
  budgetPerDay: string;
}

const EMPTY_FORM: TokenForm = { name: "", scope: "read", orgs: "", repos: "", maxConcurrent: "", budgetPerDay: "" };

/** "a, b c" → ["a", "b", "c"]: splits a list field on commas and whitespace, dropping the empties. */
export function parseList(raw: string): string[] {
  return raw.split(/[,\s]+/).filter(Boolean);
}

/** What the form POSTs: the name trimmed, list fields split and dropped when empty, blank caps
 * left absent. Call it only once `capProblem` has answered null — a cap it would refuse is
 * dropped here rather than sent, since the server's JSON layer answers a body it cannot shape
 * (a fractional `max_concurrent`) with a bare 422 that names nothing. */
export function buildNewToken(form: TokenForm): NewApiToken {
  const number = (raw: string): number | undefined => {
    const parsed = Number(raw.trim());
    return raw.trim() !== "" && Number.isFinite(parsed) ? parsed : undefined;
  };
  const orgs = parseList(form.orgs);
  const repos = parseList(form.repos);
  const maxConcurrent = number(form.maxConcurrent);
  const budgetUsdPerDay = number(form.budgetPerDay);
  return {
    name: form.name.trim(),
    scope: form.scope,
    ...(orgs.length > 0 && { orgs }),
    ...(repos.length > 0 && { repos }),
    ...(maxConcurrent !== undefined && { max_concurrent: maxConcurrent }),
    ...(budgetUsdPerDay !== undefined && { budget_usd_per_day: budgetUsdPerDay }),
  };
}

/** Why the form's caps must not be sent yet, in the server's own words, or null when they are
 * fine (blank is fine: an absent cap is uncapped). Said here rather than sent, because the
 * server refuses these as malformed JSON bodies — a 422 with less to go on than this. */
export function capProblem(form: Pick<TokenForm, "maxConcurrent" | "budgetPerDay">): string | null {
  const max = form.maxConcurrent.trim();
  if (max !== "" && (!Number.isInteger(Number(max)) || Number(max) < 1)) {
    return "max_concurrent must be a whole number, at least 1";
  }
  const budget = form.budgetPerDay.trim();
  if (budget !== "" && !(Number(budget) > 0)) {
    return "budget_usd_per_day must be a positive number of dollars";
  }
  return null;
}

/** The create answer as the list holds it: metadata only. The plaintext is dropped here, so the
 * one reveal above is the only place it exists after the answer lands. */
export function toMeta(created: CreatedApiToken): ApiTokenMeta {
  const { token: _plaintext, ...meta } = created;
  return meta;
}

/** A token's org/repo limits as one line: "all repositories" when neither is set, which is how
 * the CLI's token list spells the same thing. */
export function limitsText(token: Pick<ApiTokenMeta, "orgs" | "repos">): string {
  const parts: string[] = [];
  if (token.orgs.length > 0) parts.push(`orgs ${token.orgs.join(", ")}`);
  if (token.repos.length > 0) parts.push(`repos ${token.repos.join(", ")}`);
  return parts.length > 0 ? parts.join(" · ") : "all repositories";
}

/** A token's launch caps as one line: "no caps" when there is neither. */
export function capsText(token: Pick<ApiTokenMeta, "max_concurrent" | "budget_usd_per_day">): string {
  const parts: string[] = [];
  if (token.max_concurrent != null) parts.push(`max ${token.max_concurrent} at a time`);
  if (token.budget_usd_per_day != null) parts.push(`$${token.budget_usd_per_day}/day`);
  return parts.length > 0 ? parts.join(" · ") : "no caps";
}

/** "3h ago", or "never used" until its first request. The stamp moves at most once a minute, so a
 * busy token reads a little stale here on purpose. */
export function usedText(token: Pick<ApiTokenMeta, "last_used_at">, now: Date = new Date()): string {
  return token.last_used_at ? timeAgo(token.last_used_at, now) : "never used";
}

const SCOPE_TONE: Record<ApiTokenScope, Tone> = { fleet: "neutral", read: "neutral", operate: "info", launch: "accent" };

/** The created day, "12 Sep 2026"; an unparseable stamp passes through, like the other panes. */
function createdDay(createdAt: string): string {
  const at = new Date(createdAt);
  return Number.isNaN(at.getTime())
    ? createdAt
    : at.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

function Code({ children }: { children: ReactNode }) {
  return <code className="rounded bg-panel-3 px-1 font-mono text-meta-lg">{children}</code>;
}

/** One labelled field of the create form: label above, control, then the control's one-line hint.
 * The control is given the field's id, and a describedby pointing at the hint when there is one,
 * so a screen reader announces the hint with the field. */
function Field({ id, label, hint, children }: { id: string; label: string; hint?: string; children: ReactElement<any> }) {
  return (
    <div className="space-y-1">
      <label htmlFor={id} className="text-small-lg font-medium">
        {label}
      </label>
      {cloneElement(children, { id, "aria-describedby": hint ? `${id}-hint` : undefined })}
      {hint && (
        <p className="text-meta-lg text-faint" id={`${id}-hint`}>
          {hint}
        </p>
      )}
    </div>
  );
}

/** One token in the list: name and scope badge, then limits, caps and the two dates. `actions`
 * carries the row's Revoke (or its confirm), kept out of the row so tests can render it bare. */
export function TokenRow({ token, actions }: { token: ApiTokenMeta; actions?: ReactNode }): ReactElement {
  return (
    <div className="flex items-center gap-3 border-b border-border px-3.5 py-2.5 last:border-b-0">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-body-sm font-medium">{token.name}</span>
          <Badge tone={SCOPE_TONE[token.scope]}>{token.scope}</Badge>
        </div>
        <div className="truncate text-meta-lg text-faint">
          {limitsText(token)} · {capsText(token)}
        </div>
        <div className="truncate text-meta-lg text-faint">
          created {createdDay(token.created_at)} · {token.last_used_at ? `last used ${usedText(token)}` : "never used"}
        </div>
      </div>
      {actions}
    </div>
  );
}

/** The one-time reveal of a fresh token's plaintext. Nothing can read it back — not the API, not
 * the registry file — so the box says to copy it now, and Done drops it from the screen. */
export function SecretReveal({
  created,
  onCopy,
  onDone,
}: {
  created: CreatedApiToken;
  onCopy?: () => void;
  onDone: () => void;
}): ReactElement {
  return (
    <div className="space-y-2 rounded-xl border border-border bg-panel-2 px-3.5 py-3">
      <p className="text-body-sm font-semibold">Token “{created.name}” created</p>
      <code
        className="block w-full break-all rounded-lg border border-border bg-panel px-3 py-2 font-mono text-small-lg select-all"
        aria-label={`The ${created.name} token`}
      >
        {created.token}
      </code>
      <p className="text-small-lg text-warn">This is the only time it is shown — copy it now. Nothing can read it back.</p>
      <div className="flex gap-2">
        {onCopy && (
          <Button size="sm" onClick={onCopy}>
            Copy
          </Button>
        )}
        <Button size="sm" variant="primary" onClick={onDone}>
          Done
        </Button>
      </div>
    </div>
  );
}

export function TokensPane({ back }: { back?: () => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  // null is "not fetched yet"; a failure says why instead of showing an empty list.
  const [tokens, setTokens] = useState<ApiTokenMeta[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<TokenForm>(EMPTY_FORM);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  // The create answer whose plaintext is still on screen; Done clears it for good.
  const [created, setCreated] = useState<CreatedApiToken | null>(null);
  // The revoke in flight (one at a time), and the row asked "really?" about.
  const [busy, setBusy] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .tokens()
      .then((rows) => {
        if (cancelled) return;
        setTokens(rows);
        setError(null);
      })
      .catch((e) => !cancelled && setError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);

  const create = async (event: FormEvent) => {
    event.preventDefault();
    const problem = capProblem(form);
    if (problem) {
      setCreateError(problem);
      return;
    }
    setCreating(true);
    setCreateError(null);
    try {
      const made = await api.createToken(buildNewToken(form));
      // Oldest first, like the server's list; the new token is the last row, metadata only — the
      // plaintext lives in the one reveal and nowhere else in this state.
      setTokens((rows) => [...(rows ?? []), toMeta(made)]);
      // A create working means the earlier load failure (if any) is stale: there is a list now.
      setError(null);
      setCreated(made);
      setForm(EMPTY_FORM);
    } catch (e) {
      setCreateError(errorMessage(e)); // the 400's reason names the field to fix
    } finally {
      setCreating(false);
    }
  };

  const revoke = async (token: ApiTokenMeta) => {
    setBusy(token.id);
    try {
      await api.revokeToken(token.id);
      setTokens((rows) => (rows ?? []).filter((other) => other.id !== token.id));
      setConfirming(null);
    } catch (e) {
      // Usually a stale row: someone revoked it first, and the 404 says so.
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const copySecret = async () => {
    if (!created) return;
    if (!navigator.clipboard) {
      toast("This browser has no clipboard to copy into", "error");
      return;
    }
    try {
      await navigator.clipboard.writeText(created.token);
      toast("Token copied");
    } catch {
      toast("Couldn't copy the token", "error");
    }
  };

  const scopeHint = TOKEN_SCOPES.find((s) => s.id === form.scope)?.hint;

  return (
    <Pane
      title="API tokens"
      subtitle="Named keys for CLIs, agents and CI, without handing out the owner token"
      info={
        <>
          <p>
            A token does what its scope allows and nothing more, and only inside any org and repo limits you set. The
            mothership stores a hash, never the token, so a leaked one is revoked here and replaced — and a created one
            is read off the screen once.
          </p>
          <p className="text-muted">
            The same work from a terminal is <Code>colonizer token create|list|revoke</Code>. A token's last-used stamp
            moves at most once a minute.
          </p>
        </>
      }
      back={back}
    >
      <div className="space-y-4">
        {created && <SecretReveal created={created} onCopy={() => void copySecret()} onDone={() => setCreated(null)} />}

        <div>
          <h4 className="mb-1.5 text-small-lg font-semibold">Tokens</h4>
          {!tokens && !error && (
            <p className="flex items-center gap-2 text-body-sm text-muted">
              <Spinner /> Loading…
            </p>
          )}
          {error && <p className="text-small-lg text-warn">Couldn’t read the tokens: {error}</p>}
          {tokens !== null && tokens.length === 0 && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
              No tokens yet. One lets a script or a CI job watch or drive this install without holding the owner token —
              what <Code>colonizer token create</Code> does, from here.
            </p>
          )}
          {tokens !== null && tokens.length > 0 && (
            <div className="overflow-hidden rounded-xl border border-border">
              {tokens.map((token) => (
                <TokenRow
                  key={token.id}
                  token={token}
                  actions={
                    confirming === token.id ? (
                      <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
                        <span className="text-small-lg text-muted">Revoke {token.name}?</span>
                        <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => void revoke(token)}>
                          {busy === token.id && <Spinner className="size-3" />}
                          Confirm
                        </Button>
                        <Button size="sm" disabled={busy !== null} onClick={() => setConfirming(null)}>
                          Cancel
                        </Button>
                      </div>
                    ) : (
                      <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => setConfirming(token.id)}>
                        Revoke
                      </Button>
                    )
                  }
                />
              ))}
            </div>
          )}
        </div>

        <div className="border-t border-border pt-4">
          <h4 className="mb-1 text-small-lg font-semibold">Create a token</h4>
          <form className="space-y-3" onSubmit={(event) => void create(event)}>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field id="token-name" label="Name">
                <input className={inputClass} value={form.name} maxLength={120} placeholder="ci" onChange={(e) => setForm({ ...form, name: e.target.value })} />
              </Field>
              <Field id="token-scope" label="Scope" hint={scopeHint}>
                <select
                  className={cx(inputClass, "cursor-pointer")}
                  value={form.scope}
                  onChange={(e) => setForm({ ...form, scope: e.target.value as ApiTokenScope })}
                >
                  {TOKEN_SCOPES.map((scope) => (
                    <option key={scope.id} value={scope.id}>
                      {scope.id}
                    </option>
                  ))}
                </select>
              </Field>
              <Field id="token-orgs" label="Orgs">
                <input className={inputClass} value={form.orgs} placeholder="acme, globex — blank means all" onChange={(e) => setForm({ ...form, orgs: e.target.value })} />
              </Field>
              <Field id="token-repos" label="Repos">
                <input className={inputClass} value={form.repos} placeholder="acme/webshop — blank means all" onChange={(e) => setForm({ ...form, repos: e.target.value })} />
              </Field>
              <Field id="token-max-concurrent" label="Max concurrent colonies">
                <input className={inputClass} value={form.maxConcurrent} inputMode="numeric" placeholder="uncapped" onChange={(e) => setForm({ ...form, maxConcurrent: e.target.value })} />
              </Field>
              <Field id="token-budget" label="Budget USD/day">
                <input className={inputClass} value={form.budgetPerDay} inputMode="decimal" placeholder="uncapped" onChange={(e) => setForm({ ...form, budgetPerDay: e.target.value })} />
              </Field>
            </div>
            {createError && (
              <p role="alert" className="text-small-lg text-err">
                {createError}
              </p>
            )}
            <Button type="submit" variant="primary" disabled={creating || form.name.trim() === ""}>
              {creating && <Spinner />}
              Create token
            </Button>
          </form>
        </div>
      </div>
    </Pane>
  );
}
