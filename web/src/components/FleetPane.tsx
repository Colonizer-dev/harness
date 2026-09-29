// Settings → Fleet (issue #686, docs/fleet.md): pairing a mothership into another's fleet, like
// phone pairing. A member's credential is the pairing-minted `fleet` token — never shown here, only
// used. The pane reads GET /api/fleet, which answers everything at once, so every mutation re-reads it.
import { cloneElement, useEffect, useState, type FormEvent, type ReactElement, type ReactNode } from "react";

import { errorMessage, useApi, useToast } from "../context";
import { formatBytes } from "../cockpit/host";
import type { CreatedFleetInvite, FleetJoinStatus, FleetMemberHealth, FleetMemberHealthState, FleetPending, FleetRole, FleetState, FleetSyncPreview } from "../types";
import { Pane } from "./SettingsDialog";
import { Badge, Button, Spinner, cx, inputClass, timeAgo, type Tone } from "./ui";

/** "123456" → "123 456": the confirmation code in the two groups both screens show. */
export function spacedCode(code: string): string {
  return /^\d{6}$/.test(code) ? `${code.slice(0, 3)} ${code.slice(3)}` : code;
}

/** "expires in 12 min" for an open invite or a pending request; "expired" once past it. */
export function expiresText(expiresAt: string, now: Date = new Date()): string {
  const minutes = Math.round((new Date(expiresAt).getTime() - now.getTime()) / 60_000);
  return minutes <= 0 ? "expired" : `expires in ${minutes} min`;
}

/** The joined day, "12 Sep 2026"; an unparseable stamp passes through, like the other panes. */
export function joinedDay(joinedAt: string): string {
  const at = new Date(joinedAt);
  return Number.isNaN(at.getTime()) ? joinedAt : at.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

/** Run a fleet mutation and hand back the error text — the pane toasts it, like TokensPane's revoke — or null when it worked. */
export async function runFleet(run: () => Promise<unknown>): Promise<string | null> {
  try {
    await run();
    return null;
  } catch (e) {
    return errorMessage(e);
  }
}

const ROLE_TONE: Record<FleetRole, Tone> = { owner: "accent", member: "info", none: "neutral" };
const ROLE_LABEL: Record<FleetRole, string> = { owner: "Owner", member: "Member", none: "Not in a fleet" };

const HEALTH_TONE: Record<FleetMemberHealthState, Tone> = { ok: "ok", unknown: "neutral", degraded: "warn", stopped: "err" };
const HEALTH_LABEL: Record<FleetMemberHealthState, string> = { ok: "OK", unknown: "Not checked yet", degraded: "Degraded", stopped: "Stopped" };

/** "Token revoked: re-pair this machine" — the badge's hover text; null when there is nothing to say. */
export function healthText(health: FleetMemberHealth): string | null {
  if (!health.reason) return null;
  return health.hint ? `${health.reason}: ${health.hint}` : health.reason;
}

/** A member's health (issue #764): the state's colour, the reason beside it, the hint on hover. */
export function MemberHealthBadge({ health }: { health?: FleetMemberHealth }) {
  if (!health) return null;
  const text = healthText(health);
  return (
    <Badge tone={HEALTH_TONE[health.state]} title={text ?? undefined}>
      {health.reason ?? HEALTH_LABEL[health.state]}
    </Badge>
  );
}

/** What "Codes match" reports when the owner has not said yes — or said no, or the invite died. */
const JOIN_NOTE: Record<Exclude<FleetJoinStatus, "joined">, string> = {
  pending: "The owner has not approved yet. Check both screens show the same code, wait a moment, and try again.",
  rejected: "The owner rejected this request. Cancel and start again with a fresh invite.",
  expired: "The invite expired. Cancel and start again with a fresh invite.",
};

function Code({ children }: { children: ReactNode }) {
  return <code className="rounded bg-panel-3 px-1 font-mono text-[11.5px]">{children}</code>;
}

/** One row of a fleet list: the info block, and the row's action(s) on the right. */
function Row({ children, actions }: { children: ReactNode; actions: ReactNode }): ReactElement {
  return (
    <div className="flex items-center gap-3 border-b border-border px-3.5 py-2.5 last:border-b-0">
      <div className="min-w-0 flex-1">{children}</div>
      {actions}
    </div>
  );
}

/** The "really?" half of a destructive action — Revoke, Remove, Leave — asked before it runs. */
function Confirm({ question, busy, onConfirm, onCancel }: { question: string; busy: boolean; onConfirm: () => void; onCancel: () => void }): ReactElement {
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-2">
      <span className="text-[12.5px] text-muted">{question}</span>
      <Button size="sm" variant="danger" disabled={busy} onClick={onConfirm}>{busy && <Spinner className="size-3" />}Confirm</Button>
      <Button size="sm" disabled={busy} onClick={onCancel}>Cancel</Button>
    </div>
  );
}

/** A titled section: heading (with an optional right-hand control), optional hint, rows — or the empty state. */
function Section({ title, hint, empty, extra, children }: { title: string; hint?: string; empty?: string; extra?: ReactNode; children?: ReactNode }): ReactElement {
  return (
    <div className="border-t border-border pt-4">
      <div className={cx("flex items-center justify-between gap-2", extra ? "mb-1.5" : "mb-1")}>
        <h4 className="text-[12.5px] font-semibold">{title}</h4>
        {extra}
      </div>
      {hint && <p className="mb-1.5 text-[11.5px] text-faint">{hint}</p>}
      {children && children !== true ? children : empty !== undefined && <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">{empty}</p>}
    </div>
  );
}

/** One labelled field of the join form, laid out like TokensPane's: label above, control, hint. */
function Field({ id, label, hint, children }: { id: string; label: string; hint?: string; children: ReactElement<any> }) {
  return (
    <div className="space-y-1">
      <label htmlFor={id} className="text-[12.5px] font-medium">{label}</label>
      {cloneElement(children, { id, "aria-describedby": hint ? `${id}-hint` : undefined })}
      {hint && <p className="text-[11.5px] text-faint" id={`${id}-hint`}>{hint}</p>}
    </div>
  );
}

/** The one-time reveal of a fresh invite's code — the registry keeps only its hash, so this screen is all there is. */
export function InviteReveal({ invite, onDone }: { invite: CreatedFleetInvite; onDone: () => void }): ReactElement {
  return (
    <div className="space-y-2 rounded-xl border border-border bg-panel-2 px-3.5 py-3">
      <p className="text-[13px] font-semibold">Invite created</p>
      <code className="block w-full break-all rounded-lg border border-border bg-panel px-3 py-2 font-mono text-[13px] select-all" aria-label="The invite code">
        {invite.code}
      </code>
      <p className="text-[12.5px] text-warn">
        Shown only now — give it to the joining machine's operator together with this cockpit's URL. It works once, and {expiresText(invite.expires_at)}.
      </p>
      <Button size="sm" variant="primary" onClick={onDone}>Done</Button>
    </div>
  );
}

/** One pending request: who wants in, the code both screens must show, and — in `actions` — the owner's decision. */
export function PendingRow({ request, actions }: { request: FleetPending; actions?: ReactNode }): ReactElement {
  return (
    <Row actions={actions}>
      <div className="flex items-center gap-2">
        <span className="truncate text-[13px] font-medium">{request.name}</span>
        {request.url && <span className="truncate font-mono text-[11.5px] text-faint">{request.url}</span>}
      </div>
      <div className="font-mono text-[15px] font-semibold tabular-nums tracking-widest" aria-label="The confirmation code to compare">
        {spacedCode(request.confirm_code)}
      </div>
      <div className="truncate text-[11.5px] text-faint">{expiresText(request.expires_at)}</div>
    </Row>
  );
}

/**
 * The member's history-push consent (issue #762): off at every join. What turning it on would send
 * — the preview's counts and bytes — sits beside the switch, so the yes is given knowing what leaves.
 */
export function HistorySync({ on, preview, previewError, busy, onToggle }: {
  on: boolean;
  preview: FleetSyncPreview | null;
  previewError?: string | null;
  busy: boolean;
  onToggle: (enabled: boolean) => void;
}): ReactElement {
  return (
    <div className="space-y-2 rounded-xl border border-border bg-panel-2 px-3.5 py-3">
      <p className="flex items-center gap-2 text-[13px] font-semibold">
        History sync <Badge tone={on ? "ok" : "neutral"}>{on ? "On" : "Off"}</Badge>
      </p>
      {preview ? (
        <p className="text-[12.5px] text-muted">
          {preview.colonies} finished {preview.colonies === 1 ? "colony" : "colonies"} and {preview.payloads} log {preview.payloads === 1 ? "file" : "files"},{" "}
          {formatBytes(preview.total_bytes)} in all{on ? "" : " would go"} to the owner · {preview.pending_colonies} not yet sent ({formatBytes(preview.pending_bytes)}).
        </p>
      ) : previewError ? (
        <p className="text-[12.5px] text-warn">Couldn’t read what would be sent: {previewError}</p>
      ) : (
        <p className="flex items-center gap-2 text-[12.5px] text-muted"><Spinner className="size-3" /> Reading what would be sent…</p>
      )}
      {preview && <p className="text-[11.5px] text-faint">Never sent: {preview.excludes}.</p>}
      {!on && <p className="text-[11.5px] text-faint">Joining a fleet sends nothing until you turn this on; leaving and re-joining turns it off again.</p>}
      <Button size="sm" variant={on ? "secondary" : "primary"} disabled={busy || (!on && !preview)} onClick={() => onToggle(!on)}>
        {busy && <Spinner className="size-3" />}
        {on ? "Stop sending history" : "Send history to the owner"}
      </Button>
    </div>
  );
}

const EMPTY_JOIN = { ownerUrl: "", code: "", name: "", url: "" };

export function FleetPane({ back, initial }: { back?: () => void; /** Pre-seeded state for tests, which run no effects; the live pane fetches. */ initial?: FleetState | null }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [fleet, setFleet] = useState<FleetState | null>(initial ?? null);
  const [error, setError] = useState<string | null>(null);
  // The mutation in flight (one at a time), and the row — or Leave — asked "really?" about.
  const [busy, setBusy] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  // The create answer whose code is still on screen; Done clears it for good.
  const [created, setCreated] = useState<CreatedFleetInvite | null>(null);
  // The join form, and how the last join step went.
  const [form, setForm] = useState(EMPTY_JOIN);
  const [joinBusy, setJoinBusy] = useState(false);
  const [joinError, setJoinError] = useState<string | null>(null);
  const [joinNote, setJoinNote] = useState<string | null>(null);
  // What the history push would send, read once this mothership is a member.
  const [syncPreview, setSyncPreview] = useState<FleetSyncPreview | null>(null);
  const [syncPreviewError, setSyncPreviewError] = useState<string | null>(null);
  const memberId = fleet?.membership?.member_id ?? null;
  const historyOn = fleet?.membership?.history_sync ?? false;

  useEffect(() => {
    if (!memberId) return;
    let cancelled = false;
    setSyncPreviewError(null);
    api
      .fleetSyncPreview()
      .then((preview) => !cancelled && setSyncPreview(preview))
      .catch((e) => !cancelled && setSyncPreviewError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api, memberId, historyOn]);

  useEffect(() => {
    let cancelled = false;
    api
      .fleet()
      .then((state) => {
        if (cancelled) return;
        setFleet(state);
        setError(null);
      })
      .catch((e) => !cancelled && setError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);

  const refresh = async () => {
    setFleet(await api.fleet());
    setError(null);
  };

  /** A fire-and-forget mutation: run it, re-read the fleet so every section moves together, and toast the failure — usually a row someone else already decided. The answer, when one is needed, comes back. */
  const act = async <T,>(key: string, run: () => Promise<T>): Promise<T | null> => {
    setBusy(key);
    let value: T | null = null;
    try {
      value = await run();
      setConfirming(null);
      await refresh();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
    }
    return value;
  };

  const join = async (event: FormEvent) => {
    event.preventDefault();
    setJoinBusy(true);
    setJoinError(null);
    setJoinNote(null);
    const optional = (v: string) => v.trim() || undefined;
    try {
      await api.joinFleet({ owner_url: form.ownerUrl.trim(), code: form.code.trim(), name: optional(form.name), url: optional(form.url) });
      await refresh();
    } catch (e) {
      setJoinError(errorMessage(e)); // a bad, used or expired code is one 404; already-in-a-fleet is the 409
    } finally {
      setJoinBusy(false);
    }
  };

  /** "Codes match": asks the owner's decision; `pending` is the normal answer while they have not approved yet. */
  const confirmJoin = async () => {
    setJoinBusy(true);
    setJoinError(null);
    try {
      const { status } = await api.confirmFleetJoin();
      await refresh();
      setJoinNote(status === "joined" ? null : JOIN_NOTE[status]);
    } catch (e) {
      setJoinError(errorMessage(e));
    } finally {
      setJoinBusy(false);
    }
  };

  /** A destructive row's right side: the button, or — once asked — its Confirm/Cancel half. */
  const ask = (key: string, label: string, question: string, run: () => Promise<unknown>): ReactElement =>
    confirming === key ? (
      <Confirm question={question} busy={busy !== null} onConfirm={() => void act(key, run)} onCancel={() => setConfirming(null)} />
    ) : (
      <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => setConfirming(key)}>{label}</Button>
    );

  const role = fleet?.role ?? null;

  return (
    <Pane
      title="Fleet"
      subtitle="Let other machines join this one, or join another's fleet"
      info={
        <>
          <p>
            A fleet is a set of motherships with one owner: members host their own colonies and appear in each other's fleet view. Joining is
            phone pairing — a single-use invite, then both screens show the same six-digit code, and membership ends from either side. A member's
            token reaches the fleet routes only, never this cockpit's own credential.
          </p>
          <p className="text-muted">The read-only <Code>COLONIZER_FLEET_PEERS</Code> setting keeps working for machines that do not pair. Details in <Code>docs/fleet.md</Code>.</p>
        </>
      }
      back={back}
    >
      <div className="space-y-4">
        {!fleet && !error && <p className="flex items-center gap-2 text-[13px] text-muted"><Spinner /> Loading…</p>}
        {error && <p className="text-[12.5px] text-warn">Couldn’t read the fleet: {error}</p>}
        {fleet && role && (
          <>
            <p className="flex items-center gap-2 text-[12.5px] text-muted">This mothership is <Badge tone={ROLE_TONE[role]}>{ROLE_LABEL[role]}</Badge></p>

            {created && <InviteReveal invite={created} onDone={() => setCreated(null)} />}

            {role === "owner" && (
              <>
                <Section
                  title="Invites"
                  empty="No open invites. Create one and give its code to the operator of the machine that wants in."
                  extra={
                    // The answer's code exists only in the reveal below; the list rows carry the id alone.
                    <Button size="sm" variant="primary" disabled={busy !== null}
                      onClick={() => void act("invite", () => api.createFleetInvite()).then((made) => made && setCreated(made))}
                    >
                      {busy === "invite" && <Spinner className="size-3" />}
                      Create invite
                    </Button>
                  }
                >
                  {fleet.invites.length > 0 && (
                    <div className="overflow-hidden rounded-xl border border-border">
                      {fleet.invites.map((invite) => (
                        <Row key={invite.id} actions={ask(invite.id, "Revoke", "Revoke this invite?", () => api.deleteFleetInvite(invite.id))}>
                          <p className="truncate text-[12.5px] text-muted">Invite <Code>{invite.id}</Code> · {expiresText(invite.expires_at)}</p>
                        </Row>
                      ))}
                    </div>
                  )}
                </Section>

                <Section
                  title="Pending requests"
                  empty="Nothing waiting. A machine that redeems one of your invites appears here with its own copy of the code."
                  hint={fleet.pending.length > 0 ? "Approve only if the joining machine shows the same code." : undefined}
                >
                  {fleet.pending.length > 0 && (
                    <div className="overflow-hidden rounded-xl border border-border">
                      {fleet.pending.map((request) => (
                        <PendingRow
                          key={request.id}
                          request={request}
                          actions={
                            <div className="flex shrink-0 gap-2">
                              <Button size="sm" variant="primary" disabled={busy !== null} onClick={() => void act(`approve-${request.id}`, () => api.approveFleetPending(request.id))}>
                                {busy === `approve-${request.id}` && <Spinner className="size-3" />}
                                Approve
                              </Button>
                              <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => void act(`reject-${request.id}`, () => api.rejectFleetPending(request.id))}>Reject</Button>
                            </div>
                          }
                        />
                      ))}
                    </div>
                  )}
                </Section>

                <Section title="Members" empty="No members yet.">
                  {fleet.members.length > 0 && (
                    <div className="overflow-hidden rounded-xl border border-border">
                      {fleet.members.map((member) => (
                        <Row
                          key={member.id}
                          actions={ask(member.id, "Remove", `Remove ${member.name}?`, () => api.removeFleetMember(member.id))}
                        >
                          <div className="flex min-w-0 items-center gap-2">
                            <span className="truncate text-[13px] font-medium">{member.name}</span>
                            <MemberHealthBadge health={member.health} />
                          </div>
                          <div className="truncate text-[11.5px] text-faint">{member.url ? `${member.url} · ` : ""}joined {joinedDay(member.joined_at)}</div>
                          {member.health?.hint && member.health.state !== "ok" && (
                            <div className="truncate text-[11.5px] text-muted">{member.health.hint}</div>
                          )}
                          {member.health?.note && <div className="truncate text-[11.5px] text-faint">{member.health.note}</div>}
                        </Row>
                      ))}
                    </div>
                  )}
                </Section>
              </>
            )}

            {role === "none" &&
              (fleet.joining ? (
                <div className="space-y-2 rounded-xl border border-border bg-panel-2 px-3.5 py-3">
                  <p className="text-[13px] font-semibold">Joining <Code>{fleet.joining.owner_url}</Code></p>
                  <div className="font-mono text-[22px] font-semibold tabular-nums tracking-[0.3em]" aria-label="The confirmation code">
                    {spacedCode(fleet.joining.confirm_code)}
                  </div>
                  <p className="text-[12.5px] text-muted">
                    Show this to the owner. Press <strong>Codes match</strong> only when their screen shows the same six digits.
                  </p>
                  {joinNote && <p role="status" className="text-[12.5px] text-warn">{joinNote}</p>}
                  {joinError && <p role="alert" className="text-[12.5px] text-err">{joinError}</p>}
                  <div className="flex gap-2">
                    <Button size="sm" variant="primary" disabled={joinBusy} onClick={() => void confirmJoin()}>{joinBusy && <Spinner className="size-3" />}Codes match</Button>
                    <Button size="sm" disabled={joinBusy} onClick={() => void act("join-cancel", () => api.cancelFleetJoin())}>Cancel</Button>
                  </div>
                </div>
              ) : (
                <form className="space-y-3 border-t border-border pt-4" onSubmit={(event) => void join(event)}>
                  <h4 className="text-[12.5px] font-semibold">Join a fleet</h4>
                  <p className="text-[11.5px] text-faint">Ask the owner for their cockpit's URL and an invite code; both screens will then show a matching code.</p>
                  <div className="grid gap-3 sm:grid-cols-2">
                    <Field id="fleet-owner-url" label="Owner's URL">
                      <input className={inputClass} value={form.ownerUrl} placeholder="http://10.0.0.5:7878" onChange={(e) => setForm({ ...form, ownerUrl: e.target.value })} />
                    </Field>
                    <Field id="fleet-code" label="Invite code">
                      <input className={cx(inputClass, "font-mono")} value={form.code} placeholder="the code the owner sent" onChange={(e) => setForm({ ...form, code: e.target.value })} />
                    </Field>
                    <Field id="fleet-name" label="This machine's name" hint="Optional — what the owner's member list calls this machine.">
                      <input className={inputClass} value={form.name} maxLength={120} placeholder="studio-2" onChange={(e) => setForm({ ...form, name: e.target.value })} />
                    </Field>
                    <Field id="fleet-url" label="This machine's URL" hint="Optional — lets the owner poll this machine's fleet view.">
                      <input className={inputClass} value={form.url} placeholder="http://10.0.0.6:7878" onChange={(e) => setForm({ ...form, url: e.target.value })} />
                    </Field>
                  </div>
                  {joinError && <p role="alert" className="text-[12.5px] text-err">{joinError}</p>}
                  <Button type="submit" variant="primary" disabled={joinBusy || form.ownerUrl.trim() === "" || form.code.trim() === ""}>
                    {joinBusy && <Spinner />}
                    Join fleet
                  </Button>
                </form>
              ))}

            {role === "member" && fleet.membership && (
              <Section title="Membership">
                <div className="space-y-2">
                  <p className="text-[12.5px] text-muted">Member of the fleet at <Code>{fleet.membership.owner_url}</Code> since {joinedDay(fleet.membership.joined_at)} ({timeAgo(fleet.membership.joined_at)}).</p>
                  <p className="text-[11.5px] text-faint">Leaving revokes this machine's fleet token and updates the mesh; every local colony and setting stays.</p>
                  {ask("leave", "Leave fleet", "Leave the fleet?", () => api.leaveFleet())}
                  <HistorySync
                    on={fleet.membership.history_sync}
                    preview={syncPreview}
                    previewError={syncPreviewError}
                    busy={busy === "history"}
                    onToggle={(enabled) => void act("history", () => api.setFleetHistorySync(enabled))}
                  />
                </div>
              </Section>
            )}
          </>
        )}
      </div>
    </Pane>
  );
}
