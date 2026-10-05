import { useEffect, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import type { LoginItemStatus } from "../../types";
import { Button, Spinner, Switch } from "../ui";
import { isSafari, runningStandalone, useInstallPrompt } from "../../installApp";
import { IosHomeScreenSheet, showIosInstallHint } from "../IosHomeScreenSheet";
import { Code, Pane, Row } from "./ui";

// ---------------------------------------------------------------------------
// Desktop: the cockpit as an installed app, and the mothership started at login.
// ---------------------------------------------------------------------------

export function DesktopPane({ back }: { back?: () => void }) {
  const api = useApi();
  const toast = useToast();
  const { available, install } = useInstallPrompt();
  const standalone = runningStandalone();
  const [login, setLogin] = useState<LoginItemStatus | null>(null);
  const [loginError, setLoginError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .loginItem()
      .then((s) => !cancelled && setLogin(s))
      .catch((e) => !cancelled && setLoginError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);

  const setEnabled = async (enabled: boolean) => {
    setSaving(true);
    try {
      const next = await api.setLoginItem(enabled);
      setLogin(next);
      toast(enabled ? "The mothership now starts when you log in" : "It no longer starts at login; the running mothership keeps running");
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Pane title="Desktop" subtitle="The cockpit as an app, and the mothership always there" back={back}>
      <div className="space-y-5">
        <section className="space-y-2">
          <h4 className="text-small-lg font-semibold">Install the cockpit as an app</h4>
          {standalone ? (
            <p className="text-small-lg text-muted">You are using the installed app.</p>
          ) : available ? (
            <div className="flex flex-wrap items-center gap-3">
              <Button variant="primary" onClick={() => void install()}>
                Install app
              </Button>
              <span className="text-small-lg text-muted">Its own window and Dock/taskbar icon; same cockpit, same sign-in.</span>
            </div>
          ) : showIosInstallHint() ? (
            // On iOS the install and the push story are the same story: Add to Home Screen.
            <IosHomeScreenSheet />
          ) : isSafari() ? (
            <p className="text-small-lg text-muted">
              In Safari: <Code>File → Add to Dock</Code>. It opens in its own window with the Colonizer icon.
            </p>
          ) : (
            <p className="text-small-lg text-muted">
              Your browser has not offered to install yet. In Chrome or Edge use the install icon in the address bar (or{" "}
              <Code>⋮ → Cast, save and share → Install page as app</Code>); in Safari, <Code>File → Add to Dock</Code>.
            </p>
          )}
        </section>

        <section className="space-y-2">
          <h4 className="text-small-lg font-semibold">Start at login</h4>
          {loginError ? (
            <p className="text-small-lg text-err">{loginError}</p>
          ) : !login ? (
            <p className="flex items-center gap-2 text-body-sm text-muted">
              <Spinner /> Loading…
            </p>
          ) : login.platform === "unsupported" ? (
            <p className="text-small-lg text-muted">Start at login is available on macOS and Linux.</p>
          ) : (
            <>
              <Row id="login-item-switch" label="Start Colonizer at login" inline>
                <Switch
                  id="login-item-switch"
                  labelledBy="login-item-switch-label"
                  label="Start Colonizer at login"
                  checked={login.enabled}
                  disabled={saving}
                  onChange={(checked) => void setEnabled(checked)}
                />
              </Row>
              <p className="text-small-lg text-muted">
                {login.platform === "macos" ? "A LaunchAgent" : "A systemd user unit"} runs <Code>{login.binary}</Code> when you log in and
                restarts it only if it crashes; it logs to <Code>{login.log}</Code>.{" "}
                {login.pid ? `Running now as pid ${login.pid}.` : ""} Turning it off never stops the running mothership or its colonies.
                Same as <Code>colonizer login-item enable|disable</Code>.
              </p>
              {login.note && <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">{login.note}</p>}
            </>
          )}
        </section>
      </div>
    </Pane>
  );
}
