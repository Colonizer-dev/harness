// The "Your cockpit" card (issue #867): where this cockpit can be reached, in plain words, each
// address with Copy and a QR code. What it shows is bookmarkable()'s output — scheme, host and base
// path only — so the one-time sign-in link in the terminal (?token=…) and a phone pairing code (#…)
// can never leak into a bookmark. It reads the remote-access view for the "Anywhere" address, and
// GET /api/phone for the mothership's ranked origins, so the network/tailnet address is named
// without minting an invite (issue #867; older motherships omit the field, and cockpitAddresses then
// falls back to the address this page is already open on).
import { useEffect, useState, type ReactElement } from "react";

import { bookmarkShortcut, clearBookmark, cockpitAddresses, currentAddress, networkGap, type CockpitAddress } from "../cockpitAddress";
import { useApi, useToast } from "../context";
import { useInstallPrompt } from "../installApp";
import type { PhoneOrigin, RemoteStatus } from "../types";
import { QrCode } from "./RemoteAccessPane";
import { Button } from "./ui";

export function YourCockpitCard({
  remote: initialRemote = null,
  origins: initialOrigins,
  here,
  onOpenPhone,
}: {
  /** Seed the card before its own GET /api/remote; static tests render with it. */
  remote?: RemoteStatus | null;
  /** Seed the card before its own GET /api/phone; static tests render with it. */
  origins?: readonly PhoneOrigin[];
  /** Override this page's own address; defaults to `window.location`. */
  here?: string;
  /** Opens the pairing flow (Settings → Add your phone), when this card is not already there. */
  onOpenPhone?: () => void;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const install = useInstallPrompt();
  const [remote, setRemote] = useState<RemoteStatus | null>(initialRemote);
  const [origins, setOrigins] = useState<readonly PhoneOrigin[] | undefined>(initialOrigins);
  const [address] = useState(() => here ?? currentAddress());
  const [showQr, setShowQr] = useState<CockpitAddress["kind"] | null>(null);

  // The remote view only decides whether there is an "Anywhere" address; a failure just leaves it off.
  useEffect(() => {
    let cancelled = false;
    api
      .remote()
      .then((view) => !cancelled && setRemote(view))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [api]);

  // The ranked origins name the network/tailnet address. An old server omits them, which just
  // leaves the `here` fallback in cockpitAddresses; a failure is treated the same way.
  useEffect(() => {
    let cancelled = false;
    api
      .phones()
      .then((view) => !cancelled && setOrigins(view.origins ?? []))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [api]);

  const addresses = cockpitAddresses({ here: address, origins, remote });
  // Only the loopback was offered: say plainly that nothing here is reachable from another device.
  const gap = addresses.length > 0 ? networkGap(addresses, origins ?? []) : null;

  const copy = async (one: CockpitAddress) => {
    if (!navigator.clipboard) {
      toast("This browser has no clipboard to copy into", "error");
      return;
    }
    try {
      await navigator.clipboard.writeText(one.url);
      toast(`${one.label} address copied`);
    } catch {
      toast("Couldn't copy the address", "error");
    }
  };

  // The card's own re-offer: forget the remembered "no", then install if the browser is offering it.
  const addToDevice = () => {
    clearBookmark();
    if (install.available) void install.install();
    else toast(`Press ${bookmarkShortcut()} to bookmark this page`);
  };

  return (
    <div className="space-y-4">
      <p className="text-[12.5px] text-muted">
        The sign-in link in your terminal works once; this address doesn&rsquo;t change and has no password in it, so bookmark it.
      </p>
      {addresses.length === 0 ? (
        <p className="text-[12.5px] text-muted">This cockpit has no address to offer yet.</p>
      ) : (
        <ul className="space-y-3">
          {addresses.map((one) => (
            <li key={one.kind} className="rounded-xl border border-border px-3.5 py-3">
              <div className="flex flex-wrap items-center gap-2">
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] font-semibold">{one.label}</div>
                  <code className="block break-all font-mono text-[12.5px] text-muted select-all" aria-label={`${one.label} address`}>
                    {one.url}
                  </code>
                </div>
                <Button size="sm" onClick={() => void copy(one)}>
                  Copy
                </Button>
                <Button size="sm" aria-expanded={showQr === one.kind} onClick={() => setShowQr(showQr === one.kind ? null : one.kind)}>
                  {showQr === one.kind ? "Hide QR" : "Show QR"}
                </Button>
              </div>
              {one.note && (
                <p role="note" className="mt-2 text-[12px] text-warn">
                  {one.note}
                </p>
              )}
              {showQr === one.kind && (
                <div className="mt-3">
                  <QrCode text={one.url} />
                  <p className="mt-1.5 text-[12px] text-faint">Point another device&rsquo;s camera here to open this address.</p>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
      {gap && (
        <p role="note" className="rounded-xl border border-warn/25 bg-warn-soft px-3.5 py-2.5 text-[12.5px] text-warn">
          {gap}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {onOpenPhone && (
          <Button size="sm" onClick={onOpenPhone}>
            Put it on your phone
          </Button>
        )}
        <Button size="sm" onClick={addToDevice}>
          Add to this device
        </Button>
      </div>
    </div>
  );
}
