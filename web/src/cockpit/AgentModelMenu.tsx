// The agent and model new colonies start on (the cockpit's top-right pill, and the launch dialog's
// "change" link, which open this same menu). It lists only the models the install has enabled for
// each installed agent module that can launch, grouped under the module's name, and "Manage models…"
// opens Settings → Models. A Claude model on Pi or Claude Code adds an account picker ("Org default"
// sends no account) and, on Pi, the note about extra usage. Keyboard: the trigger is a button with
// aria-haspopup="menu"; the arrow keys move between items, Enter or Space picks one, Escape closes and
// returns focus to the trigger.
import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore, type KeyboardEvent, type ReactElement } from "react";

import { errorMessage, useApi } from "../context";
import { cx } from "../components/ui";
import {
  PI_CLAUDE_NOTE,
  checkedChoice,
  getLaunchSelection,
  launchGroups,
  launchSummary,
  setLaunchSelection,
  subscribeLaunchSelection,
  takesClaudeAccount,
  type LaunchChoice,
  type LaunchSelection,
} from "../features/models/launchAgent";
import type { ClaudeAccount, ModelAssignments } from "../features/models/types";

export interface AgentModelMenuProps {
  /** `pill` sits in the header bar; `inline` is the launch dialog's row with a "Change" button. */
  variant: "pill" | "inline";
  /** Opens Settings → Models, from the menu's "Manage models…" entry. */
  onManage: () => void;
  /** For the tests, which have no API: the assignments in hand. */
  initialAssignments?: ModelAssignments;
  /** For the tests: the Claude accounts in hand. */
  initialAccounts?: ClaudeAccount[];
  initialOpen?: boolean;
}

const ITEM = "flex w-full cursor-pointer items-center gap-2 rounded-md border-0 bg-transparent px-2.5 py-1.5 text-left text-body-sm text-text hover:bg-panel-2 focus-visible:bg-panel-2 focus-visible:outline-none aria-disabled:cursor-not-allowed aria-disabled:text-faint";

/** The items a keyboard can move between, in document order. */
function itemsOf(menu: HTMLElement | null): HTMLElement[] {
  return menu ? Array.from(menu.querySelectorAll<HTMLElement>('[role="menuitemradio"], [role="menuitem"]')) : [];
}

export function AgentModelMenu({ variant, onManage, initialAssignments, initialAccounts, initialOpen = false }: AgentModelMenuProps): ReactElement {
  const api = useApi();
  const [assignments, setAssignments] = useState<ModelAssignments | null>(initialAssignments ?? null);
  const [accounts, setAccounts] = useState<ClaudeAccount[]>(initialAccounts ?? []);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(initialOpen);
  const sel = useSyncExternalStore(subscribeLaunchSelection, getLaunchSelection, getLaunchSelection);
  const trigger = useRef<HTMLButtonElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const accountId = useId();

  const load = useCallback(() => {
    api
      .modelAssignments()
      .then((list) => {
        setAssignments(list);
        setError(null);
      })
      .catch((e: unknown) => setError(errorMessage(e)));
    // An older mothership has no accounts route: the picker then offers only the org's account.
    api
      .claudeAccounts()
      .then(setAccounts)
      .catch(() => setAccounts([]));
  }, [api]);

  useEffect(() => {
    if (!initialAssignments) load();
  }, [load, initialAssignments]);

  const close = useCallback((returnFocus: boolean) => {
    setOpen(false);
    if (returnFocus) trigger.current?.focus();
  }, []);

  const show = () => {
    setOpen(true);
    if (!initialAssignments) load();
  };

  // Focus the checked item (or the first) when the menu opens.
  useEffect(() => {
    if (!open) return;
    const items = itemsOf(menu.current);
    (items.find((el) => el.getAttribute("aria-checked") === "true") ?? items[0])?.focus();
  }, [open, assignments]);

  // A click outside closes the menu.
  useEffect(() => {
    if (!open) return;
    const outside = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", outside);
    return () => window.removeEventListener("mousedown", outside);
  }, [open]);

  const groups = assignments ? launchGroups(assignments) : [];
  const anyLaunchable = assignments ? assignments.modules.some((m) => m.blocked === null) : false;
  const install = assignments ? { module: assignments.install.module, model: assignments.install.roles.find((r) => r.role === "model")?.value ?? "" } : null;
  const checked = assignments && install ? checkedChoice(groups, sel, install) : null;
  const summary = assignments ? launchSummary(sel, assignments) : null;
  const label = summary ? `${summary.name} · ${summary.model}` : "Agent";
  const isChecked = (c: LaunchChoice) => checked?.module === c.module && checked?.model === c.model;

  // A Claude model on Pi or Claude Code names an account; Pi also gets the note on what it bills as.
  const pickAccount = checked !== null && takesClaudeAccount(checked.module, checked.model);
  const onPi = pickAccount && checked?.module === "pi";

  const setAccount = (value: string) => {
    if (!checked) return;
    const next: LaunchSelection = { module: checked.module, model: checked.model };
    if (value) next.account = value;
    setLaunchSelection(next);
  };

  const choose = (c: LaunchChoice) => {
    if (c.disabled) return;
    setLaunchSelection({ module: c.module, model: c.model });
    close(true);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const items = itemsOf(menu.current);
    const at = items.indexOf(e.target as HTMLElement);
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        items[(at + 1) % items.length]?.focus();
        break;
      case "ArrowUp":
        e.preventDefault();
        items[(at - 1 + items.length) % items.length]?.focus();
        break;
      case "Home":
        e.preventDefault();
        items[0]?.focus();
        break;
      case "End":
        e.preventDefault();
        items[items.length - 1]?.focus();
        break;
      case "Escape":
        e.preventDefault();
        close(true);
        break;
      case "Tab":
        close(false);
        break;
      case "Enter":
      case " ":
        if (at >= 0) {
          e.preventDefault();
          items[at].click();
        }
        break;
    }
  };

  const openButton = (
    <button
      ref={trigger}
      type="button"
      aria-haspopup="menu"
      aria-expanded={open}
      aria-controls={open ? menuId : undefined}
      onClick={() => (open ? close(true) : show())}
      {...(variant === "pill"
        ? {
            "aria-label": `New colonies run on ${label} · change`,
            title: "Choose the agent and model new colonies run on",
            className:
              "inline-flex h-8 max-w-[14rem] cursor-pointer items-center gap-1.5 rounded-full border border-border bg-transparent px-2.5 text-small-lg font-medium text-text transition-colors hover:border-border-strong hover:bg-panel-2",
          }
        : {
            className: "shrink-0 cursor-pointer rounded-md border-0 bg-transparent px-2 py-1 text-small font-medium text-accent hover:bg-accent-soft",
          })}
    >
      {variant === "pill" ? (
        <>
          <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-ok" />
          <span className="truncate">{label}</span>
          <span aria-hidden="true" className="text-micro-lg text-faint">▾</span>
        </>
      ) : (
        "Change"
      )}
    </button>
  );

  // The account picker and, on Pi, the note. Outside the menu's own list so the arrow keys stay with the select.
  const accountSection = pickAccount && checked && (
    <div className="space-y-1.5 px-2.5 py-2">
      <label htmlFor={accountId} className="block text-small font-medium text-muted">
        Claude account
      </label>
      <select
        id={accountId}
        value={sel?.account ?? ""}
        onChange={(e) => setAccount(e.target.value)}
        className="w-full cursor-pointer rounded-md border border-border bg-panel px-2 py-1.5 text-body-sm text-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
      >
        <option value="">Org default</option>
        {accounts.map((a) => (
          <option key={a.id} value={a.id}>
            {a.label}
            {a.is_default ? " (default)" : ""}
          </option>
        ))}
      </select>
      {onPi && <p className="text-small leading-snug text-muted">{PI_CLAUDE_NOTE}</p>}
    </div>
  );

  const menuList = (
    <div ref={menu} id={menuId} role="menu" aria-label="Agent and model for new colonies" onKeyDown={onKeyDown} className="max-h-[60vh] overflow-y-auto">
      {error && <p className="px-2.5 py-2 text-body-sm text-err">{error}</p>}
      {!assignments && !error && <p className="px-2.5 py-2 text-body-sm text-muted">Loading models…</p>}
      {assignments && !anyLaunchable && <p className="px-2.5 py-2 text-body-sm text-muted">No agent module can launch on this install yet.</p>}
      {assignments && anyLaunchable && groups.length === 0 && (
        <p className="px-2.5 py-2 text-body-sm text-muted">No model is enabled for an agent yet. Enable some under Manage models.</p>
      )}
      {groups.map((g) => (
        <div key={g.module} role="group" aria-labelledby={`${menuId}-${g.module}`} className="pt-1">
          <div id={`${menuId}-${g.module}`} className="px-2.5 pb-1 pt-1.5 text-small font-medium text-muted">
            {g.name}
          </div>
          {g.choices.map((c) => (
            <button
              key={c.model}
              type="button"
              role="menuitemradio"
              aria-checked={isChecked(c)}
              aria-disabled={c.disabled || undefined}
              tabIndex={-1}
              onClick={() => choose(c)}
              className={ITEM}
            >
              <span className="min-w-0 flex-1 truncate">{c.modelLabel}</span>
              {c.reason && <span className="shrink-0 text-small text-warn">{c.reason}</span>}
              <span aria-hidden="true" className="w-3 shrink-0 text-accent">
                {isChecked(c) ? "✓" : ""}
              </span>
            </button>
          ))}
        </div>
      ))}
      <div className="mt-1 border-t border-border pt-1">
        <button
          type="button"
          role="menuitem"
          tabIndex={-1}
          onClick={() => {
            close(false);
            onManage();
          }}
          className={ITEM}
        >
          Manage models…
        </button>
      </div>
    </div>
  );

  const panel = open && (
    <div
      onKeyDown={(e) => {
        // Escape also works from the account picker, which sits outside the menu's own key handling.
        if (e.key === "Escape") {
          e.preventDefault();
          close(true);
        }
      }}
      className={cx(
        "absolute z-50 w-72 max-w-[calc(100vw-2rem)] rounded-xl border border-border-strong bg-panel p-1.5 shadow-[0_16px_48px_rgb(0_0_0/0.35)]",
        variant === "pill" ? "right-0 top-10" : "left-0 top-full mt-1.5",
      )}
    >
      {menuList}
      {variant === "pill" && accountSection && <div className="border-t border-border">{accountSection}</div>}
    </div>
  );

  if (variant === "pill") {
    return (
      <div ref={root} className="relative shrink-0">
        {openButton}
        {panel}
      </div>
    );
  }
  return (
    <div ref={root} className="relative">
      <div className="flex items-center gap-2 rounded-lg border border-border bg-panel py-1.5 pl-3 pr-1.5">
        <span className="shrink-0 text-small text-muted">Runs on</span>
        <span className="min-w-0 flex-1 truncate text-body font-medium">{label}</span>
        {openButton}
      </div>
      {accountSection && <div className="mt-1.5 rounded-lg border border-border bg-panel">{accountSection}</div>}
      {panel}
    </div>
  );
}
