// Settings → Notifications → the push device list (issue #743): every device the mothership will
// wake, each with its last report, a test push, and prefs of its own — which events, which repos,
// which hours stay quiet. Any device's prefs are editable from any device (the mothership keeps one
// blob per subscription); only a save made from this device claims its timezone, because only here
// is it known.
import { useState, type ReactElement } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { deviceTz, mergePushPrefs, minutesToTime, timeToMinutes, validScopeEntry } from "../push";
import type { OrgInfo, PushEventKind, PushPrefs, PushSubscriptionSummary } from "../types";
import { ChipsInput, Row } from "./SettingsDialog";
import { Button, Spinner, Switch, cx, inputClass, timeAgo } from "./ui";

/** The events a device can be told about, in the editor's order, with its plain labels. */
const EVENTS: ReadonlyArray<readonly [PushEventKind, string]> = [
  ["question", "Questions"],
  ["pull_request", "Pull request opened"],
  ["needs_rebase", "Needs rebase"],
  ["failed", "Failed"],
  ["attention", "Needs attention"],
  ["provider_degraded", "Provider degraded"],
  ["digest", "Hourly digest"],
];

/** How a device's notifications look, beside which events it gets at all. */
const TOGGLES: ReadonlyArray<readonly ["question_sound" | "answer_actions" | "badge", string]> = [
  ["question_sound", "Play a sound for questions"],
  ["answer_actions", "Answer buttons on questions"],
  ["badge", "Needs-you count on the app icon"],
];

/** An enrolled device's date, as the list shows it: "Sep 23". */
function pushDate(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** "42m ago", or "never" until this device's first presence report. */
function lastSeenText(lastSeen: number | null): string {
  return lastSeen ? timeAgo(new Date(lastSeen * 1000).toISOString()) : "never";
}

/** The device name, renamed in place: the label turns into an input, Enter or blur saves it. */
function DeviceName({ row, onRenamed }: { row: PushSubscriptionSummary; onRenamed: (row: PushSubscriptionSummary) => void }) {
  const api = useApi();
  const toast = useToast();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(row.label);

  const rename = async () => {
    const label = draft.trim();
    setEditing(false);
    if (!label || label === row.label) return;
    try {
      onRenamed(await api.updatePushSubscription(row.id, { label }));
    } catch (error) {
      toast(errorMessage(error), "error");
    }
  };

  if (!editing) {
    return (
      <button
        type="button"
        title="Rename"
        onClick={() => {
          setDraft(row.label);
          setEditing(true);
        }}
        className="cursor-pointer truncate text-left text-[13px] font-medium hover:text-accent"
      >
        {row.label}
      </button>
    );
  }
  return (
    <input
      /* Autofocused, and committed on Enter or blur like the dialog's other one-field edits. */
      autoFocus
      value={draft}
      aria-label="Device name"
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => void rename()}
      onKeyDown={(e) => {
        if (e.key === "Enter") void rename();
        if (e.key === "Escape") setEditing(false);
      }}
      className={cx(inputClass, "max-w-60 py-1 text-[13px]")}
    />
  );
}

/** One device's prefs, expanded beneath its row. The draft starts from the row's stored prefs with
 * every gap filled from the defaults, and is saved wholesale, like the server takes it. */
export function PushDeviceEditor({
  row,
  orgs,
  isThisDevice,
  onSaved,
}: {
  row: PushSubscriptionSummary;
  orgs?: OrgInfo[];
  /** Whether this browser is the device in the row: only then does a save stamp the timezone. */
  isThisDevice: boolean;
  onSaved: (row: PushSubscriptionSummary) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  // A row with no prefs at all (an older mothership) reads as the defaults.
  const [prefs, setPrefs] = useState<PushPrefs>(() => mergePushPrefs(row.prefs));
  const [quietOn, setQuietOn] = useState(prefs.quiet != null);
  const [startText, setStartText] = useState(minutesToTime(prefs.quiet?.start ?? 1320));
  const [endText, setEndText] = useState(minutesToTime(prefs.quiet?.end ?? 480));
  const [saving, setSaving] = useState(false);
  const patch = (partial: Partial<PushPrefs>) => setPrefs((previous) => ({ ...previous, ...partial }));
  const id = (key: string) => `push-${row.id}-${key}`;

  const invalid = prefs.scope.filter((entry) => !validScopeEntry(entry));
  const suggestions = (orgs ?? []).map((info) => info.org).filter((org) => !prefs.scope.includes(org)).slice(0, 6);

  const save = async () => {
    const start = timeToMinutes(startText);
    const end = timeToMinutes(endText);
    if (quietOn && (start === null || end === null || start === end)) {
      toast("Quiet hours need two different times of day.", "error");
      return;
    }
    setSaving(true);
    try {
      const clock = isThisDevice ? deviceTz() : { tz: prefs.tz, utc_offset: prefs.utc_offset };
      const next: PushPrefs = {
        ...prefs,
        quiet: quietOn && start !== null && end !== null ? { start, end } : null,
        ...clock,
      };
      onSaved(await api.updatePushSubscription(row.id, { prefs: next }));
      toast(`Saved ${row.label}'s notification prefs.`);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-1 border-t border-border bg-panel-2/40 px-3.5 py-2">
      {EVENTS.map(([key, label]) => (
        <Row key={key} id={id(key)} label={label} inline>
          <Switch
            id={id(key)}
            labelledBy={`${id(key)}-label`}
            label={label}
            checked={prefs.events[key] ?? false}
            onChange={(checked) => patch({ events: { ...prefs.events, [key]: checked } })}
          />
        </Row>
      ))}
      {TOGGLES.map(([key, label]) => (
        <Row key={key} id={id(key)} label={label} inline>
          <Switch
            id={id(key)}
            labelledBy={`${id(key)}-label`}
            label={label}
            checked={prefs[key]}
            onChange={(checked) => patch({ [key]: checked })}
          />
        </Row>
      ))}

      <div className="space-y-1 py-2">
        <label htmlFor={id("scope")} className="text-[13.5px] font-medium">
          Repositories
        </label>
        <ChipsInput
          id={id("scope")}
          values={prefs.scope}
          onChange={(scope) => patch({ scope })}
          placeholder={prefs.scope.length ? "Add another" : "All repos — add an org or org/repo to narrow"}
        />
        {invalid.length > 0 && <p className="text-[11.5px] text-err">Not an org or org/repo: {invalid.join(", ")}</p>}
        {suggestions.length > 0 && (
          <p className="flex flex-wrap items-center gap-1.5 text-[11.5px] text-faint">
            Known orgs:
            {suggestions.map((org) => (
              <button
                key={org}
                type="button"
                onClick={() => patch({ scope: [...prefs.scope, org] })}
                className="cursor-pointer rounded bg-panel-3 px-1.5 py-0.5 font-mono hover:text-text"
              >
                {org}
              </button>
            ))}
          </p>
        )}
      </div>

      <Row id={id("quiet")} label="Quiet hours" inline>
        <Switch id={id("quiet")} labelledBy={`${id("quiet")}-label`} label="Quiet hours" checked={quietOn} onChange={setQuietOn} />
      </Row>
      {quietOn && (
        <div className="flex flex-wrap items-center gap-2 py-1 text-[13.5px]">
          <span className="font-medium">From</span>
          <input type="time" value={startText} aria-label="Quiet from" onChange={(e) => setStartText(e.target.value)} className={cx(inputClass, "w-28")} />
          <span className="font-medium">to</span>
          <input type="time" value={endText} aria-label="Quiet to" onChange={(e) => setEndText(e.target.value)} className={cx(inputClass, "w-28")} />
          <span className="text-[12px] text-faint">in {prefs.tz ?? "the device's own timezone"}</span>
        </div>
      )}
      <Row id={id("break")} label="Questions break through quiet hours" inline>
        <Switch
          id={id("break")}
          labelledBy={`${id("break")}-label`}
          label="Questions break through quiet hours"
          checked={prefs.questions_break_quiet}
          disabled={!quietOn}
          onChange={(checked) => patch({ questions_break_quiet: checked })}
        />
      </Row>

      <div className="flex items-center gap-2 pt-1">
        <Button size="sm" variant="primary" disabled={saving} onClick={() => void save()}>
          {saving && <Spinner className="size-3" />}
          Save
        </Button>
      </div>
    </div>
  );
}

/** The rows themselves: name and last report, the three actions, and one editor open at a time. */
export function PushDeviceList({
  subs,
  orgs,
  ownIds,
  onRevoke,
  onChanged,
}: {
  subs: PushSubscriptionSummary[];
  orgs?: OrgInfo[];
  /** The rows that are this browser's own subscription — the saves that may claim its timezone. */
  ownIds: ReadonlySet<string>;
  onRevoke: (row: PushSubscriptionSummary) => void;
  onChanged: (row: PushSubscriptionSummary) => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [openId, setOpenId] = useState<string | null>(null);
  const [testing, setTesting] = useState<string | null>(null);

  const sendTest = (row: PushSubscriptionSummary) => {
    if (testing) return;
    setTesting(row.id);
    api
      .testPushSubscription(row.id)
      .then(({ sent }) =>
        toast(
          sent ? `Test push on its way to ${row.label}.` : `${row.label} could not be reached — the push service refused it.`,
          sent ? "success" : "error",
        ),
      )
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => setTesting(null));
  };

  return (
    <div className="mt-1 overflow-hidden rounded-xl border border-border">
      {subs.map((row) => (
        <div key={row.id} className="border-b border-border last:border-b-0">
          <div className="flex items-center gap-3 px-3.5 py-2.5">
            <div className="min-w-0 flex-1">
              <DeviceName row={row} onRenamed={onChanged} />
              <div className="truncate font-mono text-[11px] text-faint">
                {row.endpoint_host} · enrolled {pushDate(row.created_at)} · last seen {lastSeenText(row.last_seen)}
              </div>
            </div>
            <Button size="sm" disabled={testing !== null} onClick={() => sendTest(row)}>
              {testing === row.id && <Spinner className="size-3" />}
              Send test
            </Button>
            <Button size="sm" aria-expanded={openId === row.id} onClick={() => setOpenId((open) => (open === row.id ? null : row.id))}>
              {openId === row.id ? "Hide prefs" : "Prefs"}
            </Button>
            <Button variant="danger" size="sm" onClick={() => onRevoke(row)}>
              Revoke
            </Button>
          </div>
          {openId === row.id && (
            <PushDeviceEditor key={row.id} row={row} orgs={orgs} isThisDevice={ownIds.has(row.id)} onSaved={onChanged} />
          )}
        </div>
      ))}
    </div>
  );
}
