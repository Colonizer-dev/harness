// Settings → Remote access (issue #535): the switch behind the relay tunnel of remote.rs
// (docs/protocol.md §6.10), the link and its QR code, the live link status, the pairing codes
// waiting to be confirmed, and the reset that retires a leaked link. The switch state itself is
// owned by App — the top bar's badge reads the same view — so every answer is folded back up
// through `onChanged`.
import { useEffect, useRef, useState, type ReactElement } from "react";
import { encode } from "uqr";

import { errorMessage, useApi, useToast } from "../context";
import type { RemotePairing, RemoteStatus } from "../types";
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

/** The live link's one line: green with a local time while connected, amber while it redials. */
export function connectionText(remote: RemoteStatus): string {
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

export function RemoteAccessPane({
  remote,
  onChanged,
  back,
  initialPairing,
}: {
  /** The view App holds; null until the first GET /api/remote has answered. */
  remote: RemoteStatus | null;
  /** Folds a PUT/reset answer back into App's state, so the top bar's badge moves with it. */
  onChanged: (remote: RemoteStatus) => void;
  back?: () => void;
  /** Seeds the pairing block before the pane's own first fetch; static tests render with it. */
  initialPairing?: RemotePairing | null;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [showQr, setShowQr] = useState(false);
  const [askReset, setAskReset] = useState(false);
  // undefined has no meaning here: null is "this mothership has no pairing endpoint yet" (#534),
  // which hides the block, exactly like "not fetched yet" does.
  const [pairing, setPairing] = useState<RemotePairing | null>(initialPairing ?? null);
  const [confirming, setConfirming] = useState<string | null>(null);
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
  // appear on App's next 30 s refresh. A resolved null (no endpoint yet, #534) hides the block;
  // a thrown failure keeps whatever is on screen — a poll must not blank it or toast.
  useEffect(() => {
    if (!remote?.enabled) return;
    let cancelled = false;
    api
      .remotePairing()
      .then((view) => !cancelled && setPairing(view))
      .catch(() => {});
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

  const confirmCode = async (code: string) => {
    setConfirming(code);
    try {
      const done = await api.confirmRemotePairing(code);
      toast(`Paired with @${done.owner.github_login}`);
    } catch (e) {
      toast(errorMessage(e), "error"); // a bad code (400), a gone one (404), an owner already bound (409)
    } finally {
      if (alive.current) setConfirming(null);
    }
    // Refetched either way: a confirm binds the owner and clears every pending code at the relay,
    // and a 409 means the view here is stale. A failed refetch keeps the view, and a late answer
    // never reaches a closed pane.
    try {
      const view = await api.remotePairing();
      if (alive.current) setPairing(view);
    } catch {
      /* keep what is on screen */
    }
  };

  const link = remote ? remoteLink(remote) : null;

  return (
    <Pane title="Remote access" subtitle="Open this cockpit from your phone or another computer" back={back}>
      {!remote ? (
        <p className="flex items-center gap-2 text-[13px] text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <Row id="remote-switch" label="Allow remote access" inline>
            <Switch
              id="remote-switch"
              labelledBy="remote-switch-label"
              label="Allow remote access"
              checked={remote.enabled}
              disabled={saving}
              onChange={(checked) => void toggle(checked)}
            />
          </Row>
          <div className="space-y-2 text-[12.5px] text-muted">
            <p>
              While on, this machine dials out an encrypted tunnel to the Colonizer relay and serves this cockpit on a link of its
              own. Nothing listens on a public port here, and switching off drops the tunnel and every request in flight at once.
            </p>
            <p>
              The link only works for you: it asks for a GitHub sign-in, and the first one waits for a six-digit code that you
              confirm here; the cockpit behind it then asks for its own access token, as it would for any new browser. What it
              exposes is this cockpit — everything you can see and do here, colony terminals included — and nothing else on this
              machine: no other port or service, and no file or shell access beyond what the cockpit itself offers. The relay
              routes the traffic and stores no request data — only this install’s public key and the pairing.
            </p>
          </div>

          {remote.enabled && (
            <div className="space-y-4 border-t border-border pt-4">
              <div>
                <h4 className="mb-1.5 text-[12.5px] font-semibold">Your link</h4>
                {link ? (
                  // At phone width the link takes the whole row and the buttons sit under it. The
                  // display gets a <wbr> after every dot, so it wraps on the dots and only breaks
                  // mid-segment if one is longer than a line; Copy and the QR keep the plain link.
                  <div className="flex flex-wrap items-center gap-2">
                    <code
                      className="w-full min-w-0 break-words rounded-lg border border-border bg-panel-2 px-3 py-2 font-mono text-[12.5px] select-all text-text sm:w-auto sm:flex-1"
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
                  <p className="text-[12.5px] text-muted">The link appears once the relay has answered the registration.</p>
                )}
                {showQr && link && (
                  <div className="mt-3">
                    <QrCode text={link} />
                    <p className="mt-1.5 text-[12px] text-faint">Point a phone camera here to open the link.</p>
                  </div>
                )}
                <p role="status" className={cx("mt-2 flex items-center gap-1.5 text-[12.5px]", remote.connected ? "text-ok" : "text-warn")}>
                  <span aria-hidden="true" className={cx("size-1.5 shrink-0 rounded-full", remote.connected ? "bg-ok" : "bg-warn")} />
                  {connectionText(remote)}
                </p>
              </div>

              {pairing && (
                <div>
                  <h4 className="mb-1.5 text-[12.5px] font-semibold">Pairing</h4>
                  {pairing.owner ? (
                    <p className="text-[12.5px] text-muted">
                      Paired with @{pairing.owner.github_login} — only that GitHub account can sign in to the link.
                    </p>
                  ) : pairing.pending.length === 0 ? (
                    <p className="text-[12.5px] text-muted">
                      No pending requests. The first sign-in to the link shows a code that appears here to confirm.
                    </p>
                  ) : (
                    <>
                      <p className="mb-1.5 text-[12.5px] text-muted">
                        A sign-in to the link is waiting. Confirm only if your phone shows the same code.
                      </p>
                      <div className="overflow-hidden rounded-xl border border-border">
                        {pairing.pending.map((request) => (
                          // Code and login/expiry stacked, Confirm beside them: at phone width the
                          // meta line stays one line of its own instead of orphan-wrapping.
                          <div key={request.code} className="flex items-center gap-3 border-b border-border px-3.5 py-2.5 last:border-b-0">
                            <div className="min-w-0 flex-1">
                              <div className="font-mono text-[15px] font-semibold tabular-nums tracking-widest">{formatPairingCode(request.code)}</div>
                              <div className="truncate text-[12px] text-faint">
                                @{request.github_login} · {expiryText(request.expires_at)}
                              </div>
                            </div>
                            <Button size="sm" variant="primary" disabled={confirming !== null} onClick={() => void confirmCode(request.code)}>
                              {confirming === request.code && <Spinner className="size-3" />}
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
                {askReset ? (
                  <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-panel-2 px-3.5 py-2.5">
                    <p className="min-w-0 flex-1 text-[12.5px] text-muted">Really reset? The old link stops working.</p>
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
                    <span className="text-[12.5px] text-muted">A fresh identity and link; the old link stops working. The way out of a leaked one.</span>
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
