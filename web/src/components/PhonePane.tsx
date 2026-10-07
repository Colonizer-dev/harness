// Settings → Add your phone (issue #746). Pairing a phone is three steps, the relay pairing's model:
// this pane mints a single-use invite and shows it as a QR code; the phone that scans it shows a
// six-digit code; typing that code here approves that one phone, which then gets a credential of
// its own. The invite is a ticket to ask, never a credential, and the API token never appears in
// any QR or URL. Each paired phone is listed with a Revoke that signs it out alone.
// Which origin the QR names is a small pure choice over the mothership's preference-ordered list,
// and an unreachable or insecure pick is explained rather than hidden.
import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";

import { errorMessage, useApi, useToast } from "../context";
import { chosenOrigin } from "../phoneOrigins";
import type { PhoneInvite, PhoneOrigin, Phones } from "../types";
import { QrCode, expiryText } from "./RemoteAccessPane";
import { Pane } from "./SettingsDialog";
import { Button, Spinner, inputClass, timeAgo } from "./ui";

// Re-exported for the callers that import it from here; the pick itself lives in ../phoneOrigins so
// the Your cockpit card can share it without importing this pane.
export { chosenOrigin };

/** The url a phone camera gets: the invite in the query, on the origin's base. */
export function inviteUrl(origin: Pick<PhoneOrigin, "url">, code: string): string {
  return `${origin.url}/?pair=${encodeURIComponent(code)}`;
}

/** "expires in 5 min" from the RFC3339 stamp the endpoint answers with; a stamp it cannot parse passes through. */
export function inviteExpiry(invite: Pick<PhoneInvite, "expires_at">, now: number = Date.now()): string {
  const unix = Math.floor(Date.parse(invite.expires_at) / 1000);
  return Number.isNaN(unix) ? invite.expires_at : expiryText(unix, now);
}

const KIND_LABEL: Record<PhoneOrigin["kind"], string> = { relay: "Relay link", tailnet: "Tailnet", lan: "Your network" };

export function PhonePane({
  back,
  initialInvite = null,
  initialPhones = null,
}: {
  back?: () => void;
  /** Seed the pane before its own fetches; static tests render with them. */
  initialInvite?: PhoneInvite | null;
  initialPhones?: Phones | null;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [invite, setInvite] = useState<PhoneInvite | null>(initialInvite);
  const [phones, setPhones] = useState<Phones | null>(initialPhones);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const refresh = useCallback(() => {
    api
      .phones()
      .then((view) => alive.current && setPhones(view))
      .catch(() => {}); // a poll keeps what is on screen
  }, [api]);

  // On open, and every few seconds while an invite is out: a scanned invite turns into a phone
  // waiting for its code, which should appear here without a reload.
  useEffect(() => {
    refresh();
    if (!invite) return;
    const timer = window.setInterval(refresh, 3000);
    return () => window.clearInterval(timer);
  }, [refresh, invite]);

  const run = async (key: string, action: () => Promise<string | null>) => {
    setBusy(key);
    try {
      const message = await action();
      if (message) toast(message);
      setError(null);
    } catch (e) {
      if (alive.current) setError(errorMessage(e));
    } finally {
      if (alive.current) setBusy(null);
      refresh();
    }
  };

  const mint = () =>
    run("mint", async () => {
      const fresh = await api.phoneInvite();
      if (alive.current) setInvite(fresh);
      return null;
    });

  const confirm = () =>
    run("confirm", async () => {
      const { label } = await api.confirmPhone(code);
      if (alive.current) {
        setCode("");
        setInvite(null); // spent: the phone that opened it is the one just approved
      }
      return `${label} is signed in`;
    });

  const reject = (id: string) =>
    run(`reject:${id}`, async () => {
      await api.rejectPhone(id);
      return "Turned that phone down";
    });

  const revoke = (id: string, label: string) =>
    run(`revoke:${id}`, async () => {
      await api.revokePhone(id);
      return `${label} is signed out`;
    });

  const origins = invite?.origins ?? [];
  const origin = chosenOrigin(origins);
  const noneReachable = origins.length > 0 && !origins.some((o) => o.reachable);
  const pending = phones?.pending ?? [];
  const devices = phones?.devices ?? [];

  return (
    <Pane title="Add your phone" subtitle="Pair your phone by scanning a code, then confirming it here" back={back}>
      <div className="space-y-5">
        {!invite ? (
          <div className="space-y-3">
            <p className="text-small-lg text-muted">
              Shows a single-use QR code. Scan it with your phone, then type the six-digit code the phone shows into this pane. The phone
              gets a sign-in of its own that you can revoke here at any time; the code expires in five minutes.
            </p>
            <Button variant="primary" disabled={busy !== null} onClick={() => void mint()}>
              {busy === "mint" && <Spinner className="size-3" />}
              Show a code to scan
            </Button>
          </div>
        ) : (
          <div className="space-y-4">
            <div className="flex flex-wrap items-start gap-4">
              {origin && <QrCode text={inviteUrl(origin, invite.code)} />}
              <div className="min-w-0 flex-1 space-y-1.5 text-small-lg text-muted">
                <p>1. Point your phone's camera here and open the link.</p>
                <p>2. Type the code your phone then shows:</p>
                <form
                  className="flex items-center gap-2 pt-0.5"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void confirm();
                  }}
                >
                  <input
                    aria-label="Code shown on your phone"
                    inputMode="numeric"
                    autoComplete="off"
                    placeholder="123 456"
                    value={code}
                    onChange={(event) => setCode(event.target.value)}
                    className={`${inputClass} w-32 font-mono`}
                  />
                  <Button type="submit" size="sm" variant="primary" disabled={busy !== null || code.replace(/\D/g, "").length !== 6}>
                    {busy === "confirm" && <Spinner className="size-3" />}
                    Confirm
                  </Button>
                </form>
                {origin && (
                  <p className="pt-1">
                    <span className="font-medium text-text">{KIND_LABEL[origin.kind]}</span> · <span className="break-all font-mono text-small">{origin.url}</span>
                  </p>
                )}
                <p>{inviteExpiry(invite)} · single use</p>
                <Button size="sm" disabled={busy !== null} onClick={() => void mint()}>
                  New code
                </Button>
              </div>
            </div>

            {noneReachable && (
              <div role="note" className="space-y-1 rounded-xl border border-warn/25 bg-warn-soft px-3.5 py-2.5 text-small-lg text-warn">
                <p className="font-medium">Your phone can't reach this machine from outside yet.</p>
                {origins.map((o) => o.note && <p key={o.url}>{o.note}</p>)}
                <p>Turning on Remote access (Settings → Remote access) gives the phone an https link that works from anywhere.</p>
              </div>
            )}
            {origins.length === 0 && (
              <p role="note" className="rounded-xl border border-warn/25 bg-warn-soft px-3.5 py-2.5 text-small-lg text-warn">
                No address a phone could open was found. Turn on Remote access (Settings → Remote access) for an https link that works
                from anywhere.
              </p>
            )}
            {origin && origin.reachable && !origin.secure && (
              <p role="note" className="rounded-xl border border-warn/25 bg-warn-soft px-3.5 py-2.5 text-small-lg text-warn">
                This address is plain http and only works on {origin.kind === "tailnet" ? "your tailnet" : "your network"}: the pairing
                crosses it unencrypted, and the phone can't install the app or get notifications over it. The relay link from Remote
                access has neither problem.
              </p>
            )}
            <p className="text-small text-faint">
              The installed app belongs to the address it was installed from: pairing through a different one means installing the app
              again.
            </p>
          </div>
        )}

        {error && <p className="text-small-lg text-err">{error}</p>}

        {pending.length > 0 && (
          <div>
            <h4 className="mb-1.5 text-small-lg font-semibold">Waiting for their code</h4>
            <ul className="space-y-1.5">
              {pending.map((p) => (
                <li key={p.id} className="flex items-center gap-2 text-small-lg">
                  <span className="min-w-0 flex-1">
                    {p.label} <span className="text-faint">· {inviteExpiry(p)}</span>
                  </span>
                  <Button size="sm" disabled={busy !== null} onClick={() => void reject(p.id)}>
                    Turn down
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        )}

        <div>
          <h4 className="mb-1.5 text-small-lg font-semibold">Paired phones</h4>
          {devices.length === 0 ? (
            <p className="text-small-lg text-muted">No phone is paired yet.</p>
          ) : (
            <ul className="space-y-1.5">
              {devices.map((d) => (
                <li key={d.id} className="flex items-center gap-2 text-small-lg">
                  <span className="min-w-0 flex-1">
                    {d.label} <span className="text-faint">· paired {timeAgo(d.paired_at)}</span>
                  </span>
                  <Button size="sm" disabled={busy !== null} onClick={() => void revoke(d.id, d.label)}>
                    {busy === `revoke:${d.id}` && <Spinner className="size-3" />}
                    Revoke
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </Pane>
  );
}
