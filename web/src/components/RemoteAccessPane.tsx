// Settings → Remote access (issue #535): the switch behind the relay tunnel of remote.rs
// (docs/protocol.md §6.10), the link and its QR code, the live link status, the pairing codes
// waiting to be confirmed or rejected and the bound owner to unbind (#599, all local-only at the
// mothership), whether the relay asks for GitHub sign-in before the pair code (#1086, local-only
// too), and the reset that retires a leaked link. The switch state itself is
// owned by App — the top bar's badge reads the same view — so every answer is folded back up
// through `onChanged`.
import { useEffect, useRef, useState, type ReactElement } from "react";
import { encode } from "uqr";

import { errorMessage, useApi, useToast } from "../context";
import type { LinkDevices, LinkInvite, RemotePairing, RemoteStatus } from "../types";
import { Pane, Row } from "./SettingsDialog";
import { Button, Spinner, Switch, cx } from "./ui";

/** `https://<host>`, the link the relay serves this cockpit on; null until the first enable. */
export function remoteLink(remote: Pick<RemoteStatus, "host">): string | null {
  return remote.host ? `https://${remote.host}` : null;
}

/** "123456" → "123 456": the pairing code in the two groups a phone screen shows. */
export function formatPairingCode(code: string): string {
  return /^\d{6}$/.test(code) ? `${code.slice(0, 3)} ${code.slice(3)}` : code;
}

/** The live link's one line: green with a local time while connected, amber while it redials, and
 * a takeover warning when another mothership's tunnel holds the link. */
export function connectionText(remote: RemoteStatus): string {
  if (remote.replaced) return "Another mothership took over this link";
  if (!remote.connected || !remote.since) return "Offline — reconnecting";
  const at = new Date(remote.since);
  return `Connected since ${Number.isNaN(at.getTime()) ? remote.since : at.toLocaleTimeString()}`;
}

/** "expires in 7 min" for a pending pairing; the relay counts expiries in unix seconds. */
export function expiryText(expiresAt: number, now: number = Date.now()): string {
  const minutes = Math.round((expiresAt * 1000 - now) / 60_000);
  if (minutes <= 0) return "expires now";
  return minutes < 60 ? `expires in ${minutes} min` : `expires at ${new Date(expiresAt * 1000).toLocaleTimeString()}`;
}

/** The link as a QR code, for a phone camera: uqr's dark modules as one inline SVG path. */
export function QrCode({ text, size = 168 }: { text: string; size?: number }): ReactElement {
  const qr = encode(text, { ecc: "M", border: 2 });
  // One subpath per dark run of a row, so a camera-dark link stays a single small element.
  let d = "";
  for (let y = 0; y < qr.size; y++) {
    let x = 0;
    while (x < qr.size) {
      if (!qr.data[y][x]) {
        x++;
        continue;
      }
      const start = x;
      while (x < qr.size && qr.data[y][x]) x++;
      d += `M${start} ${y}h${x - start}v1h-${x - start}z`;
    }
  }
  return (
    <span className="inline-grid shrink-0 place-items-center rounded-xl border border-border bg-white p-2">
      <svg role="img" aria-label={`QR code for ${text}`} width={size} height={size} viewBox={`0 0 ${qr.size} ${qr.size}`} shapeRendering="crispEdges">
        <path d={d} fill="black" />
      </svg>
    </span>
  );
}

/** Settings → Remote access → Sign in on another device (review finding R3). This machine's own
 * access token is never accepted through the link, so a browser elsewhere signs in the way a phone
 * pairs: a single-use link opened there shows six digits, typed here, and that browser gets a link
 * credential of its own — listed here, revocable one by one, and all rotated by Reset link. */
export function LinkDevicesBlock({ initialDevices }: { initialDevices?: LinkDevices | null }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [devices, setDevices] = useState<LinkDevices | null>(initialDevices ?? null);
  const [invite, setInvite] = useState<LinkInvite | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = async () => {
    try {
      setDevices(await api.linkDevices());
    } catch {
      /* keep what is on screen */
    }
  };
  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api]);

  const run = async (key: string, action: () => Promise<void>) => {
    setBusy(key);
    try {
      await action();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setBusy(null);
      void refresh();
    }
  };

  const start = () =>
    run("invite", async () => {
      setInvite(await api.linkInvite());
      setCode("");
    });
  const confirm = () =>
    run("confirm", async () => {
      const { label } = await api.confirmLinkDevice(code);
      setInvite(null);
      setCode("");
      toast(`Signed in: ${label}`);
    });
  const revoke = (id: string) =>
    run(`revoke:${id}`, async () => {
      await api.revokeLinkDevice(id);
      toast("Signed out of the link");
    });

  return (
    <div>
      <h4 className="mb-1.5 text-small-lg font-semibold">Sign in on another device</h4>
      <p className="mb-2 text-small-lg text-muted">
        This machine’s access token never works through the link. To use the cockpit from another computer or phone, open a
        one-time link there and type the six digits it shows here. That browser gets a sign-in of its own; Reset link signs every
        one of them out.
      </p>
      {invite ? (
        <div className="space-y-2 rounded-xl border border-border bg-panel-2 px-3.5 py-3">
          <p className="text-small-lg text-muted">Open this on the other device (it works once, for five minutes):</p>
          <code className="block break-all font-mono text-small select-all" aria-label="One-time sign-in link">
            {invite.url}
          </code>
          <QrCode text={invite.url} size={140} />
          <div className="flex flex-wrap items-center gap-2">
            <input
              aria-label="Code shown on the other device"
              inputMode="numeric"
              placeholder="123 456"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              className="w-28 rounded-lg border border-border bg-panel px-2.5 py-1.5 font-mono text-body-sm tracking-widest"
            />
            <Button size="sm" variant="primary" disabled={busy !== null || code.replace(/\D/g, "").length !== 6} onClick={() => void confirm()}>
              {busy === "confirm" && <Spinner className="size-3" />}
              Confirm
            </Button>
            <Button size="sm" disabled={busy !== null} onClick={() => setInvite(null)}>
              Cancel
            </Button>
          </div>
        </div>
      ) : (
        <Button size="sm" disabled={busy !== null} onClick={() => void start()}>
          {busy === "invite" && <Spinner className="size-3" />}
          Sign in on another device
        </Button>
      )}
      {devices && devices.devices.length > 0 && (
        <div className="mt-3 overflow-hidden rounded-xl border border-border">
          {devices.devices.map((device) => (
            <div key={device.id} className="flex items-center gap-3 border-b border-border px-3.5 py-2 last:border-b-0">
              <div className="min-w-0 flex-1 truncate text-small-lg">
                {device.label}
                <span className="text-faint"> · signed in {new Date(device.paired_at).toLocaleDateString()}</span>
              </div>
              <Button size="sm" disabled={busy !== null} onClick={() => void revoke(device.id)}>
                {busy === `revoke:${device.id}` && <Spinner className="size-3" />}
                Sign out
              </Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function RemoteAccessPane({
  remote,
  onChanged,
  back,
  initialPairing,
  initialDevices,
}: {
  /** The view App holds; null until the first GET /api/remote has answered. */
  remote: RemoteStatus | null;
  /** Folds a PUT/reset answer back into App's state, so the top bar's badge moves with it. */
  onChanged: (remote: RemoteStatus) => void;
  back?: () => void;
  /** Seeds the pairing block before the pane's own first fetch; static tests render with it. */
  initialPairing?: RemotePairing | null;
  /** Seeds the signed-in devices block, for static tests. */
  initialDevices?: LinkDevices | null;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [showQr, setShowQr] = useState(false);
  const [askReset, setAskReset] = useState(false);
  // null is "not fetched yet", which hides the block; a fetch that fails before any view arrived
  // shows its reason instead (the relay is unreachable, or no longer knows this install).
  const [pairing, setPairing] = useState<RemotePairing | null>(initialPairing ?? null);
  const [pairingError, setPairingError] = useState<string | null>(null);
  // The pairing action in flight: "confirm:<code>", "reject:<code>" or "unbind"; one at a time.
  const [busy, setBusy] = useState<string | null>(null);
  const [askUnbind, setAskUnbind] = useState(false);
  // Whether this pane is still mounted: a late confirm answer must not reach a closed one.
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // The pane refreshes the shared view on mount, so opening Settings cannot show a stale switch.
  useEffect(() => {
    let cancelled = false;
    api
      .remote()
      .then((view) => !cancelled && onChanged(view))
      .catch(() => {}); // quiet, like App's poll: no badge, no toast spam
    return () => {
      cancelled = true;
    };
  }, [api, onChanged]);

  // The pairing view only means something while the switch is on, and is keyed on the polled
  // view itself, not just the flag: a code made on the phone while this pane sits open must
  // appear on App's next 30 s refresh. A failure keeps whatever is on screen — a poll must not
  // blank it or toast — and only says why while there is nothing to show.
  useEffect(() => {
    if (!remote?.enabled) return;
    let cancelled = false;
    api
      .remotePairing()
      .then((view) => {
        if (cancelled) return;
        setPairing(view);
        setPairingError(null);
      })
      .catch((e) => !cancelled && setPairingError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api, remote]);

  const toggle = async (enabled: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setRemote(enabled));
      toast(enabled ? "Remote access is on" : "Remote access is off — the tunnel is closed");
    } catch (e) {
      // The server's own message says which failure it was: a 502 is the relay refusing or
      // missing, and the switch stays off because nothing was persisted.
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const toggleGithub = async (requireGithub: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setRemoteRequireGithub(requireGithub));
      toast(requireGithub ? "The link asks for GitHub sign-in first" : "Devices pair with the link by code alone");
    } catch (e) {
      // A 502 says the relay refused or predates the setting; nothing changed then.
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const copyLink = async (link: string) => {
    if (!navigator.clipboard) {
      toast("This browser has no clipboard to copy into", "error");
      return;
    }
    try {
      await navigator.clipboard.writeText(link);
      toast("Link copied");
    } catch {
      toast("Couldn't copy the link", "error");
    }
  };

  const resetLink = async () => {
    setSaving(true);
    try {
      onChanged(await api.resetRemote());
      setAskReset(false);
      toast("The link was reset — the old one stopped working");
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  // One pairing decision, then the relay's fresh view — refetched either way: a confirm binds the
  // owner and clears every pending code, a reject or unbind drops rows, and a 404/409 means the
  // view here is stale. A failed refetch keeps the view, and a late answer never reaches a closed
  // pane.
  const decide = async (key: string, action: () => Promise<string>) => {
    setBusy(key);
    try {
      toast(await action());
    } catch (e) {
      toast(errorMessage(e), "error"); // a bad code (400), a gone one (404), an owner already bound (409)
    } finally {
      if (alive.current) setBusy(null);
    }
    try {
      const view = await api.remotePairing();
      if (alive.current) setPairing(view);
    } catch {
      /* keep what is on screen */
    }
  };

  const confirmCode = (code: string) =>
    decide(`confirm:${code}`, async () => `Paired with @${(await api.confirmRemotePairing(code)).owner.github_login}`);

  const rejectCode = (code: string) =>
    decide(`reject:${code}`, async () => `Turned down the sign-in from @${(await api.rejectRemotePairing(code)).github_login}`);

  const unbind = () =>
    decide("unbind", async () => {
      await api.unbindRemoteOwner();
      if (alive.current) setAskUnbind(false);
      return "Unbound — the next sign-in to the link asks for a new code";
    });

  const link = remote ? remoteLink(remote) : null;

  return (
    <Pane title="Remote access" subtitle="Open this cockpit from your phone or another computer" back={back}>
      {!remote ? (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <Row id="remote-switch" label="Allow remote access" help="Opens this cockpit from your phone or another computer through the Colonizer relay." inline>
            <Switch
              id="remote-switch"
              labelledBy="remote-switch-label"
              label="Allow remote access"
              checked={remote.enabled}
              disabled={saving}
              onChange={(checked) => void toggle(checked)}
            />
          </Row>
          <div className="space-y-2 text-small-lg text-muted">
            <p>
              While on, this machine dials out an encrypted tunnel to the Colonizer relay and serves this cockpit on a link of its
              own. Nothing listens on a public port here, and switching off drops the tunnel and every request in flight at once.
            </p>
            <p>
              The link only works for devices you pair here: a one-time link opened on the device shows six digits, and nothing
              reaches this cockpit until you type them here.
              {remote.require_github
                ? " Before that, the relay also asks for a GitHub sign-in, and the first one waits for a code you confirm here."
                : " The relay lets nothing else through: a browser that is not paired gets a page telling it how to pair."}{" "}
              This machine’s own access token is never accepted through the link. What it
              exposes is this cockpit — everything you can see and do here, colony terminals included — and nothing else on this
              machine: no other port or service, and no file or shell access beyond what the cockpit itself offers. The relay
              routes the traffic and stores no request data — only this install’s public key and the pairing.
            </p>
          </div>

          <Row id="remote-github" label="Ask for GitHub sign-in first" help="Optional. On, every device signs in with GitHub before it can use the pair code; off, the pair code alone is enough." inline>
            <Switch
              id="remote-github"
              labelledBy="remote-github-label"
              label="Ask for GitHub sign-in first"
              checked={remote.require_github}
              disabled={saving}
              onChange={(checked) => void toggleGithub(checked)}
            />
          </Row>
          <p className="text-small-lg text-muted">The relay limits how often a device or the link may try, either way.</p>

          {remote.enabled && (
            <div className="space-y-4 border-t border-border pt-4">
              <div>
                <h4 className="mb-1.5 text-small-lg font-semibold">Your link</h4>
                {link ? (
                  // At phone width the link takes the whole row and the buttons sit under it. The
                  // display gets a <wbr> after every dot, so it wraps on the dots and only breaks
                  // mid-segment if one is longer than a line; Copy and the QR keep the plain link.
                  <div className="flex flex-wrap items-center gap-2">
                    <code
                      className="w-full min-w-0 break-words rounded-lg border border-border bg-panel-2 px-3 py-2 font-mono text-small-lg select-all text-text sm:w-auto sm:flex-1"
                      aria-label="Remote access link"
                    >
                      {link.split(".").flatMap((part, i) => (i === 0 ? [part] : [".", <wbr key={i} />, part]))}
                    </code>
                    <Button size="sm" onClick={() => void copyLink(link)}>
                      Copy
                    </Button>
                    <Button size="sm" aria-expanded={showQr} onClick={() => setShowQr((shown) => !shown)}>
                      {showQr ? "Hide QR" : "QR code"}
                    </Button>
                  </div>
                ) : (
                  <p className="text-small-lg text-muted">The link appears once the relay has answered the registration.</p>
                )}
                {showQr && link && (
                  <div className="mt-3">
                    <QrCode text={link} />
                    <p className="mt-1.5 text-small text-faint">Point a phone camera here to open the link.</p>
                  </div>
                )}
                <p role="status" className={cx("mt-2 flex items-center gap-1.5 text-small-lg", remote.connected ? "text-ok" : "text-warn")}>
                  <span aria-hidden="true" className={cx("size-1.5 shrink-0 rounded-full", remote.connected ? "bg-ok" : "bg-warn")} />
                  {connectionText(remote)}
                </p>
              </div>

              {!pairing && pairingError && (
                <div>
                  <h4 className="mb-1.5 text-small-lg font-semibold">Pairing</h4>
                  <p className="text-small-lg text-warn">Couldn’t read the pairing from the relay: {pairingError}</p>
                </div>
              )}

              {/* The GitHub owner binding (#534) matters only behind the GitHub gate; without it the block
                  stays while there is still an owner to unbind or a sign-in waiting. */}
              {pairing && (remote.require_github || pairing.owner || pairing.pending.length > 0) && (
                <div>
                  <h4 className="mb-1.5 text-small-lg font-semibold">Pairing</h4>
                  {pairing.owner ? (
                    <div className="space-y-2">
                      <p className="text-small-lg text-muted">
                        Paired with @{pairing.owner.github_login} —{" "}
                        {remote.require_github
                          ? "only that GitHub account can sign in to the link."
                          : "GitHub sign-in is off, so this only matters if you switch it back on."}
                      </p>
                      {askUnbind ? (
                        <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-panel-2 px-3.5 py-2.5">
                          <p className="min-w-0 flex-1 text-small-lg text-muted">
                            Unbind @{pairing.owner.github_login}? Their sign-in stops working at once.
                          </p>
                          <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => void unbind()}>
                            {busy === "unbind" && <Spinner className="size-3" />}
                            Unbind
                          </Button>
                          <Button size="sm" disabled={busy !== null} onClick={() => setAskUnbind(false)}>
                            Cancel
                          </Button>
                        </div>
                      ) : (
                        <Button size="sm" disabled={busy !== null} onClick={() => setAskUnbind(true)}>
                          Unbind
                        </Button>
                      )}
                    </div>
                  ) : pairing.pending.length === 0 ? (
                    <p className="text-small-lg text-muted">
                      No pending requests. The first sign-in to the link shows a code that appears here to confirm.
                    </p>
                  ) : (
                    <>
                      <p className="mb-1.5 text-small-lg text-muted">
                        A sign-in to the link is waiting. Confirm only if your phone shows the same code; reject it if you
                        did not just sign in.
                      </p>
                      <div className="overflow-hidden rounded-xl border border-border">
                        {pairing.pending.map((request) => (
                          // Code and login/expiry stacked, Confirm beside them: at phone width the
                          // meta line stays one line of its own instead of orphan-wrapping.
                          <div key={request.code} className="flex items-center gap-3 border-b border-border px-3.5 py-2.5 last:border-b-0">
                            <div className="min-w-0 flex-1">
                              <div className="font-mono text-lead font-semibold tabular-nums tracking-widest">{formatPairingCode(request.code)}</div>
                              <div className="truncate text-small text-faint">
                                @{request.github_login} · {expiryText(request.expires_at)}
                              </div>
                            </div>
                            <Button size="sm" disabled={busy !== null} onClick={() => void rejectCode(request.code)}>
                              {busy === `reject:${request.code}` && <Spinner className="size-3" />}
                              Reject
                            </Button>
                            <Button size="sm" variant="primary" disabled={busy !== null} onClick={() => void confirmCode(request.code)}>
                              {busy === `confirm:${request.code}` && <Spinner className="size-3" />}
                              Confirm
                            </Button>
                          </div>
                        ))}
                      </div>
                    </>
                  )}
                </div>
              )}

              <div className="border-t border-border pt-4">
                <LinkDevicesBlock initialDevices={initialDevices} />
              </div>

              <div className="border-t border-border pt-4">
                {askReset ? (
                  <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-panel-2 px-3.5 py-2.5">
                    <p className="min-w-0 flex-1 text-small-lg text-muted">Really reset? The old link stops working.</p>
                    <Button size="sm" variant="danger" disabled={saving} onClick={() => void resetLink()}>
                      {saving && <Spinner className="size-3" />}
                      Confirm
                    </Button>
                    <Button size="sm" disabled={saving} onClick={() => setAskReset(false)}>
                      Cancel
                    </Button>
                  </div>
                ) : (
                  <div className="flex flex-wrap items-center gap-3">
                    <Button size="sm" variant="danger" disabled={saving} onClick={() => setAskReset(true)}>
                      Reset link
                    </Button>
                    <span className="text-small-lg text-muted">
                      A fresh identity and link; the old link is retired at the relay, its owner unbound, and every browser signed in to it
                      signed out. The way out of a leaked one.
                    </span>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      )}
    </Pane>
  );
}
