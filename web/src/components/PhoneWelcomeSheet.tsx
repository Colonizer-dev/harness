// The sheet a scanned sign-in lands on (issue #746): the phone is signed in and already looking at
// the same cockpit as the desk, and the two things worth doing once — installing the app and
// turning on notifications — are offered where they cannot be missed. Both reuse the existing
// enable paths (installApp, push); dismissing loses nothing, and the ?welcome= that opened it is
// stripped at boot, so it never re-shows on its own.
import { useState, type ReactElement } from "react";

import { errorMessage, useApi, useToast } from "../context";
import { notificationSupport, requestNotificationPermission, type NotificationPermissionState } from "../notifications";
import { useInstallPrompt } from "../installApp";
import { deviceLabel, pushSupported, subscribeThisDevice } from "../push";
import { IosHomeScreenSheet, showIosInstallHint } from "./IosHomeScreenSheet";
import { Button, Spinner } from "./ui";

export function PhoneWelcomeSheet({ onClose }: { onClose: () => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const { available, install } = useInstallPrompt();
  const [permission, setPermission] = useState<NotificationPermissionState>(() => notificationSupport());
  const [busy, setBusy] = useState(false);

  // The same enrol Settings → Notifications does: the push path asks permission and signs this
  // device up in the one click; where push is unavailable (an http origin, an old browser) the
  // plain in-tab permission is still worth asking. Both must run inside the click.
  const enableNotifications = () => {
    if (busy) return;
    setBusy(true);
    const ask = pushSupported()
      ? subscribeThisDevice(api, deviceLabel(navigator.userAgent)).then((row) => `Push is on for ${row.label}.`)
      : requestNotificationPermission().then((outcome) => {
          if (outcome !== "granted") throw new Error("notification permission was not granted");
          return "Notifications are on.";
        });
    void ask
      .then((message) => {
        setPermission("granted");
        toast(message, "success");
      })
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => setBusy(false));
  };

  return (
    <div
      role="dialog"
      aria-label="Welcome — you are signed in"
      className="fixed inset-x-3 bottom-3 z-50 max-w-sm rounded-2xl border border-border bg-panel p-4 shadow-[var(--shadow)] max-sm:mx-auto sm:right-4"
    >
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <h2 className="text-[14px] font-semibold">You're signed in</h2>
          <p className="mt-0.5 text-[12.5px] text-muted">This phone now runs the same cockpit as your desk. Two things worth doing once:</p>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="Dismiss"
          className="-m-1 grid size-7 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
        >
          ×
        </button>
      </div>
      <div className="mt-3 space-y-2.5">
        {available ? (
          <div className="flex items-center gap-2">
            <Button size="sm" variant="primary" onClick={() => void install()}>
              Install
            </Button>
            <span className="min-w-0 flex-1 text-[12.5px] text-muted">Its own icon; same cockpit, same sign-in.</span>
          </div>
        ) : showIosInstallHint() ? (
          <IosHomeScreenSheet />
        ) : (
          <p className="text-[12.5px] text-muted">Install later from Settings → Desktop, or the browser's install icon.</p>
        )}
        {permission !== "unsupported" && permission !== "granted" && (
          <div className="flex items-center gap-2">
            <Button size="sm" disabled={busy} onClick={enableNotifications}>
              {busy && <Spinner className="size-3" />}
              Turn on notifications
            </Button>
            <span className="min-w-0 flex-1 text-[12.5px] text-muted">When a colony needs you, wherever the phone is.</span>
          </div>
        )}
      </div>
    </div>
  );
}
