// Secrets: every key the mothership holds and where it lives — the system keychain or a 0600 file —
// never the value. GET /api/secrets lists them; each row sets or replaces its value (write-only),
// removes it, or moves it between the keychain and the file. Nothing moves by itself: a file
// secret stays a file until someone presses Move. `focusId` (from `openSecrets(id)`) scrolls to a
// row and opens its input, which is how the model picker's "Set key" lands here.
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactElement,
} from "react";

import { errorMessage, useApi, useToast } from "../context";
import {
  GuideIcon,
  SectionHero,
  type Guide,
} from "../components/settingsGuide";
import { Button, Spinner, cx, inputClass, timeAgo } from "../components/ui";
import type {
  ColonySecretScope,
  SecretColonyAccess,
  SecretGroup,
  SecretLocation,
  SecretRow,
  SecretsListing,
} from "../types";

const GROUPS: { id: SecretGroup; title: string; hint: string }[] = [
  {
    id: "providers",
    title: "Model providers",
    hint: "Keys the gateway uses for routed models",
  },
  {
    id: "connections",
    title: "Connections",
    hint: "GitHub, Claude and the cockpit itself",
  },
  {
    id: "integrations",
    title: "Integrations",
    hint: "Voice, memory, notifications and add-ons",
  },
  {
    id: "colonies",
    title: "Colony secrets",
    hint: "Keys you let colonies use, swapped in only for the hosts you name",
  },
];

/** What each row's "Colonies" pill says: whether and how a colony gets the secret. */
export function colonyAccessText(access: SecretColonyAccess | undefined): {
  icon: string;
  text: string;
  tone: string;
} {
  switch (access?.kind) {
    case "gateway":
      return {
        icon: "mothership",
        text: "Via gateway · never in the VM",
        tone: "border-border text-muted",
      };
    case "injected":
      return {
        icon: "ant",
        text: `Injected for ${access.hosts.join(", ")} only`,
        tone: "border-accent/40 bg-accent-soft text-accent",
      };
    case "none":
      return {
        icon: "lock",
        text: "Not given to colonies",
        tone: "border-border text-faint",
      };
    default:
      return { icon: "lock", text: "—", tone: "border-border text-faint" };
  }
}

/** The explainer card: how a key reaches a colony, and why the colony never holds it. */
const COLONY_GUIDE: Guide = {
  icon: "ant",
  blurb:
    "How colonies use secrets: a colony never holds a key. Model keys stay in the mothership's gateway; a colony secret reaches the microVM as a placeholder, which is swapped for the real value only on TLS to the hosts you allow.",
  flow: [
    { icon: "lock", label: "Keychain" },
    { icon: "mothership", label: "Mothership", metric: "adds model keys" },
    { icon: "ant", label: "Colony", metric: "sees a placeholder" },
    { icon: "globe", label: "Allowed host", metric: "gets the value" },
  ],
};

const ICON: Record<string, string> = {
  github: "github",
  claude: "chat",
  key: "lock",
  plug: "plug",
  mic: "mic",
  memory: "memory",
  bell: "bell",
  spark: "spark",
};

const LOCATION: Record<SecretLocation, { label: string; tone: string }> = {
  keychain: { label: "Keychain", tone: "border-ok/40 bg-ok/10 text-ok" },
  file: { label: "File (0600)", tone: "border-warn/40 bg-warn/10 text-warn" },
  env: { label: "Environment", tone: "border-border text-muted" },
  unset: { label: "Not set", tone: "border-border text-faint" },
};

/** The guide for the hero card: what the page is for, as a picture. */
function guide(backend: string): Guide {
  return {
    icon: "lock",
    blurb:
      "Every key this mothership holds, kept in the system keychain when it can. Values are write-only here and never shown again.",
    flow: [
      { icon: "lock", label: backend === "none" ? "Keychain" : backend },
      { icon: "mothership", label: "Mothership" },
      { icon: "globe", label: "Provider" },
    ],
  };
}

export function SecretsView({
  focusId,
  focusRequest,
}: {
  focusId?: string;
  focusRequest?: number;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [listing, setListing] = useState<SecretsListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setListing(await api.secrets());
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  // A deep link opens that row's input once the list is in.
  useEffect(() => {
    if (focusId && listing) {
      setOpen(focusId);
      document
        .getElementById(rowDomId(focusId))
        ?.scrollIntoView({ block: "center", behavior: "smooth" });
    }
  }, [focusId, focusRequest, listing]);

  const replace = (row: SecretRow) =>
    setListing(
      (current) =>
        current && {
          ...current,
          secrets: current.secrets.map((r) => (r.id === row.id ? row : r)),
        },
    );

  const act = async (
    id: string,
    what: string,
    run: () => Promise<SecretRow>,
  ) => {
    setBusy(id);
    try {
      replace(await run());
      toast(what);
      return true;
    } catch (e) {
      toast(errorMessage(e), "error");
      return false;
    } finally {
      setBusy(null);
    }
  };

  const keychain = listing?.keychain;
  const onFile =
    listing?.secrets.filter((r) => r.editable && r.location === "file") ?? [];
  const moveAll = async () => {
    for (const row of onFile) {
      await act(row.id, `${row.label} moved to the keychain`, () =>
        api.moveSecret(row.id, "keychain"),
      );
    }
  };

  const saved =
    listing?.secrets.filter(
      (r) => r.location === "keychain" || r.location === "file",
    ).length ?? 0;

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-24 pt-10">
      <div className="mx-auto flex w-full max-w-[1080px] flex-col gap-8">
        <div>
          <h1 className="m-0 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">
            Secrets
          </h1>
          <p className="mt-2 text-[14px] text-muted">
            {listing
              ? `${saved} saved · ${onFile.length} on file · values are never shown`
              : "Reading what this mothership holds…"}
          </p>
        </div>

        <SectionHero
          guide={guide(keychain?.backend ?? "none")}
          stats={
            keychain
              ? [
                  {
                    label: keychain.backend,
                    value: keychain.available ? "Available" : "Unavailable",
                    tone: keychain.available ? "ok" : "warn",
                  },
                  {
                    label: "In keychain",
                    value: String(
                      listing?.secrets.filter((r) => r.location === "keychain")
                        .length ?? 0,
                    ),
                  },
                  {
                    label: "On file",
                    value: String(onFile.length),
                    tone: onFile.length > 0 ? "warn" : undefined,
                  },
                ]
              : []
          }
        />

        <SectionHero guide={COLONY_GUIDE} />

        {keychain && !keychain.available && (
          <div
            role="status"
            className="rounded-xl border border-warn/40 bg-warn/10 px-4 py-3 text-[13px] text-text"
          >
            <div className="font-medium">
              The keychain is not available on this host, so secrets are saved
              as 0600 files.
            </div>
            <div className="mt-1 text-muted">
              {keychain.reason ?? "No reason given."} On Linux the Secret
              Service needs a desktop session with an unlocked keyring (GNOME
              Keyring or KWallet); a mothership started over ssh usually has
              neither. Start it from a desktop session, or keep the file store.
            </div>
          </div>
        )}

        {keychain?.available && onFile.length > 0 && (
          <div className="flex flex-wrap items-center gap-3 rounded-xl border border-border bg-panel-2 px-4 py-3">
            <GuideIcon name="lock" size={16} className="text-accent" />
            <span className="min-w-0 flex-1 text-[13px]">
              {onFile.length}{" "}
              {onFile.length === 1 ? "secret is" : "secrets are"} still saved as
              a plain file. Moving keeps the value and deletes the file.
            </span>
            <Button
              variant="primary"
              disabled={busy !== null}
              onClick={moveAll}
            >
              Move all to the keychain
            </Button>
          </div>
        )}

        {error && <p className="text-[13px] text-err">{error}</p>}
        {!listing && !error && (
          <p className="flex items-center gap-2 text-[13px] text-muted">
            <Spinner /> Loading…
          </p>
        )}

        {listing &&
          GROUPS.map((group) => {
            const rows = listing.secrets.filter((r) => r.group === group.id);
            // Colony secrets always show, so there is somewhere to add the first one.
            if (rows.length === 0 && group.id !== "colonies") return null;
            return (
              <section key={group.id} aria-labelledby={`secrets-${group.id}`}>
                <div className="mb-2 flex items-baseline gap-2">
                  <h2
                    id={`secrets-${group.id}`}
                    className="m-0 text-[15px] font-semibold"
                  >
                    {group.title}
                  </h2>
                  <span className="text-[12.5px] text-faint">{group.hint}</span>
                </div>
                {group.id === "colonies" && (
                  <ColonySecretForm
                    onSaved={async (id) => {
                      await load();
                      toast(`${id.slice("colony:".length)} saved for colonies`);
                    }}
                  />
                )}
                {rows.length > 0 && (
                <ul className="divide-y divide-border overflow-hidden rounded-xl border border-border bg-panel">
                  {rows.map((row) => (
                    <SecretItem
                      key={row.id}
                      row={row}
                      keychainAvailable={Boolean(keychain?.available)}
                      busy={busy === row.id}
                      open={open === row.id}
                      onOpen={(o) => setOpen(o ? row.id : null)}
                      onSave={async (value) => {
                        const ok = await act(row.id, `${row.label} saved`, () =>
                          api.saveSecret(row.id, value),
                        );
                        if (ok) setOpen(null);
                      }}
                      onRemove={async () => {
                        setBusy(row.id);
                        try {
                          const result = await api.deleteSecret(row.id);
                          if ("removed" in result) await load();
                          else replace(result);
                          toast(`${row.label} removed`);
                        } catch (e) {
                          toast(errorMessage(e), "error");
                        } finally {
                          setBusy(null);
                        }
                      }}
                      onMove={(to) =>
                        act(
                          row.id,
                          `${row.label} moved to the ${to === "keychain" ? "keychain" : "file"}`,
                          () => api.moveSecret(row.id, to),
                        )
                      }
                    />
                  ))}
                </ul>
                )}
              </section>
            );
          })}
      </div>
    </main>
  );
}

function rowDomId(id: string): string {
  return `secret-${id.replace(/[^a-z0-9-]/gi, "-")}`;
}

function SecretItem({
  row,
  keychainAvailable,
  busy,
  open,
  onOpen,
  onSave,
  onRemove,
  onMove,
}: {
  row: SecretRow;
  keychainAvailable: boolean;
  busy: boolean;
  open: boolean;
  onOpen: (open: boolean) => void;
  onSave: (value: string) => void;
  onRemove: () => void;
  onMove: (to: "keychain" | "file") => void;
}): ReactElement {
  const [value, setValue] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const saved = row.location === "keychain" || row.location === "file";
  const loc = LOCATION[row.location];
  const access = colonyAccessText(row.colonies);

  useEffect(() => {
    if (open) input.current?.focus();
    else setValue("");
  }, [open]);

  return (
    <li id={rowDomId(row.id)} className={cx("px-4 py-3", open && "bg-panel-2")}>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <span className="grid size-9 shrink-0 place-items-center rounded-lg border border-border bg-panel-2 text-muted">
          <GuideIcon name={ICON[row.icon] ?? "lock"} size={17} />
        </span>
        <div className="min-w-0 flex-1 basis-48">
          <div className="truncate text-[13.5px] font-medium text-text">
            {row.label}
          </div>
          <div className="truncate text-[12px] text-muted">
            {row.used_by}
            {row.env && (
              <span className="text-faint">
                {" "}
                · env {row.env}
                {row.env_set ? " (set)" : ""}
              </span>
            )}
          </div>
        </div>
        <span
          className={cx(
            "inline-flex max-w-[260px] shrink-0 items-center gap-1 rounded-full border px-2 py-0.5 text-[11.5px]",
            access.tone,
          )}
          title="What colonies get of this secret"
        >
          <GuideIcon name={access.icon} size={12} />
          <span className="truncate">{access.text}</span>
        </span>
        <span
          className={cx(
            "shrink-0 rounded-full border px-2 py-0.5 text-[11.5px]",
            loc.tone,
          )}
        >
          {loc.label}
        </span>
        <span className="w-24 shrink-0 text-right text-[12px] tabular-nums text-faint">
          {row.updated_at ? timeAgo(row.updated_at) : "—"}
        </span>
        <div className="flex shrink-0 items-center gap-1.5">
          {busy && <Spinner />}
          {row.editable ? (
            <>
              <Button size="sm" disabled={busy} onClick={() => onOpen(!open)}>
                {saved ? "Replace" : "Set"}
              </Button>
              {row.location === "file" && keychainAvailable && (
                <Button
                  size="sm"
                  disabled={busy}
                  onClick={() => onMove("keychain")}
                >
                  Move to keychain
                </Button>
              )}
              {row.location === "keychain" && (
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() => onMove("file")}
                  title="Save it as a 0600 file instead"
                >
                  To file
                </Button>
              )}
              {saved && (
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={onRemove}
                >
                  Remove
                </Button>
              )}
            </>
          ) : (
            <span className="text-[12px] text-faint">
              {row.env
                ? `Set ${row.env} on the mothership`
                : "Managed by the CLI"}
            </span>
          )}
        </div>
      </div>
      {open && (
        <form
          className="mt-3 flex flex-wrap items-center gap-2 pl-[52px]"
          onSubmit={(e) => {
            e.preventDefault();
            if (value.trim()) onSave(value.trim());
          }}
        >
          <input
            ref={input}
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            placeholder={
              saved ? "New value (replaces the saved one)" : "Paste the key"
            }
            aria-label={`${row.label} value`}
            className={cx(inputClass, "min-w-0 flex-1 basis-64 font-mono")}
          />
          <Button
            type="submit"
            variant="primary"
            disabled={busy || !value.trim()}
          >
            Save
            {keychainAvailable && row.location !== "file" ? " to keychain" : ""}
          </Button>
          <Button variant="ghost" onClick={() => onOpen(false)}>
            Cancel
          </Button>
        </form>
      )}
    </li>
  );
}

/** Adds a colony secret: a name, the hosts it is for, which colonies get it, and the value. */
function ColonySecretForm({
  onSaved,
}: {
  onSaved: (id: string) => Promise<void> | void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [env, setEnv] = useState("");
  const [hosts, setHosts] = useState("");
  const [scopeKind, setScopeKind] = useState<ColonySecretScope["kind"]>("all");
  const [scopeName, setScopeName] = useState("");
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);

  const envOk = /^[A-Z_][A-Z0-9_]*$/.test(env);
  const hostList = hosts
    .split(/[\s,]+/)
    .map((h) => h.trim())
    .filter(Boolean);
  const scopeOk = scopeKind === "all" || scopeName.trim() !== "";
  const ready = envOk && hostList.length > 0 && scopeOk && value.trim() !== "";

  const scope = (): ColonySecretScope =>
    scopeKind === "org"
      ? { kind: "org", org: scopeName.trim() }
      : scopeKind === "repo"
        ? { kind: "repo", repo: scopeName.trim() }
        : { kind: "all" };

  if (!open) {
    return (
      <div className="mb-2">
        <Button size="sm" onClick={() => setOpen(true)}>
          Add colony secret
        </Button>
      </div>
    );
  }

  return (
    <form
      aria-label="add a colony secret"
      className="mb-3 grid gap-3 rounded-xl border border-border bg-panel-2 p-4 sm:grid-cols-2"
      onSubmit={async (e) => {
        e.preventDefault();
        if (!ready) return;
        setSaving(true);
        try {
          const saved = await api.saveColonySecret({
            env,
            hosts: hostList,
            scope: scope(),
            value: value.trim(),
          });
          setEnv("");
          setHosts("");
          setValue("");
          setOpen(false);
          await onSaved(saved.id);
        } catch (err) {
          toast(errorMessage(err), "error");
        } finally {
          setSaving(false);
        }
      }}
    >
      <label className="flex flex-col gap-1 text-[12.5px] text-muted">
        Variable name
        <input
          value={env}
          onChange={(e) => setEnv(e.target.value.toUpperCase())}
          placeholder="STRIPE_TEST_KEY"
          spellCheck={false}
          className={cx(inputClass, "font-mono")}
          aria-invalid={env !== "" && !envOk}
        />
      </label>
      <label className="flex flex-col gap-1 text-[12.5px] text-muted">
        Allowed hosts
        <input
          value={hosts}
          onChange={(e) => setHosts(e.target.value)}
          placeholder="api.stripe.com, files.stripe.com"
          spellCheck={false}
          className={cx(inputClass, "font-mono")}
        />
      </label>
      <label className="flex flex-col gap-1 text-[12.5px] text-muted">
        Given to
        <span className="flex gap-2">
          <select
            value={scopeKind}
            onChange={(e) => setScopeKind(e.target.value as ColonySecretScope["kind"])}
            className={cx(inputClass, "w-auto")}
          >
            <option value="all">All colonies</option>
            <option value="org">One workspace</option>
            <option value="repo">One repository</option>
          </select>
          {scopeKind !== "all" && (
            <input
              value={scopeName}
              onChange={(e) => setScopeName(e.target.value)}
              placeholder={scopeKind === "org" ? "acme" : "acme/web"}
              spellCheck={false}
              aria-label={scopeKind === "org" ? "workspace" : "repository"}
              className={cx(inputClass, "min-w-0 flex-1 font-mono")}
            />
          )}
        </span>
      </label>
      <label className="flex flex-col gap-1 text-[12.5px] text-muted">
        Value
        <input
          type="password"
          autoComplete="off"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="Paste the key"
          className={cx(inputClass, "font-mono")}
        />
      </label>
      <p className="m-0 text-[12px] text-muted sm:col-span-2">
        The colony sees the variable name and a placeholder. The real value is sent only on TLS
        requests to the hosts above, and the colony is told not to print or store it.
      </p>
      <div className="flex gap-2 sm:col-span-2">
        <Button type="submit" variant="primary" disabled={!ready || saving}>
          {saving && <Spinner />} Save colony secret
        </Button>
        <Button variant="ghost" onClick={() => setOpen(false)}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
