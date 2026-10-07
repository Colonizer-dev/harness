import { useEffect, useState, type Dispatch, type SetStateAction } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { notificationSupport, requestNotificationPermission, type NotificationPermissionState, type NotificationPrefs } from "../../notifications";
import { deviceLabel, pushSupported, subscribeThisDevice, thisDeviceSubscriptions, unsubscribeThisDevice } from "../../push";
import { PushDeviceList } from "../PushDevicePrefs";
import { IosHomeScreenSheet, showIosInstallHint } from "../IosHomeScreenSheet";
import type { OrgInfo, PushSubscriptionSummary } from "../../types";
import { Button, Spinner, Switch } from "../ui";
import { Pane, Row } from "./ui";

// ---------------------------------------------------------------------------
// Notifications: what tells a person a colony needs them when the tab is not
// in front. Client-side only — the prefs persist in localStorage (see
// notifications.ts), not through the Api, so there is nothing here to save.
// ---------------------------------------------------------------------------

export function NotificationsPane({
  prefs,
  onChanged,
  orgs,
  back,
}: {
  prefs: NotificationPrefs;
  onChanged: Dispatch<SetStateAction<NotificationPrefs>>;
  /** The workspaces the cockpit knows, offered as repo-filter suggestions for a device. */
  orgs?: OrgInfo[];
  back?: () => void;
}) {
  // The browser's answer as of the pane opening, or as of the last ask from the switch below.
  const [permission, setPermission] = useState<NotificationPermissionState>(() => notificationSupport());
  const [asked, setAsked] = useState(false);

  // Web push (issue #516): the devices the mothership will wake, and this browser's place among
  // them. The subscribe asks the browser's permission right here in the click, like the switch
  // above; a refusal or a failed enrolment is said aloud rather than leaving the row silent.
  const api = useApi();
  const toast = useToast();
  const pushable = pushSupported();
  const [subs, setSubs] = useState<PushSubscriptionSummary[] | null>(null);
  const [subscribing, setSubscribing] = useState(false);
  // Which rows are this browser's own subscription: only their saves claim its timezone (#743).
  const [ownIds, setOwnIds] = useState<ReadonlySet<string>>(new Set());

  useEffect(() => {
    if (!pushable) return;
    let cancelled = false;
    api
      .pushSubscriptions()
      .then((rows) => !cancelled && setSubs(rows))
      .catch(() => !cancelled && setSubs([]));
    thisDeviceSubscriptions(api)
      .then((rows) => !cancelled && setOwnIds(new Set(rows.map((row) => row.id))))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [api, pushable]);

  const enrolThisDevice = () => {
    if (subscribing) return;
    setSubscribing(true);
    void subscribeThisDevice(api, deviceLabel(navigator.userAgent))
      .then((row) => {
        setSubs((rows) => [...(rows ?? []).filter((other) => other.id !== row.id), row]);
        setOwnIds((ids) => new Set(ids).add(row.id));
        toast(`Push is on for ${row.label}.`, "success");
      })
      .catch((error) => toast(errorMessage(error), "error"))
      .finally(() => setSubscribing(false));
  };

  const revokeDevice = (row: PushSubscriptionSummary) => {
    // Optimistic, like the rest of the dialog's removes; the row comes back on the next visit if the revoke failed.
    setSubs((rows) => (rows ?? []).filter((other) => other.id !== row.id));
    void unsubscribeThisDevice(api, row.id, row.endpoint_host).catch(() => toast("Couldn't revoke that device.", "error"));
  };

  // Functional update: the permission answer below arrives after the dialog has kept taking
  // toggles, and a write from this render's `prefs` would silently revert them.
  const patch = (partial: Partial<NotificationPrefs>) => onChanged((previous) => ({ ...previous, ...partial }));

  // Turning browser notifications on asks the browser right here, inside the click: the prompt is
  // only shown within a user gesture, which is why this setting lives behind a button and can never
  // fire on load. The switch only comes on for "granted" — a denied or dismissed prompt is
  // explained below rather than silently swallowed.
  const setBrowser = (wanted: boolean) => {
    if (!wanted) {
      patch({ browser: false });
      return;
    }
    setAsked(true);
    void requestNotificationPermission().then((outcome) => {
      setPermission(outcome);
      patch({ browser: outcome === "granted" });
    });
  };

  const info = (
    <>
      <p>
        A colony that asks a question and then waits is otherwise quiet: a status pill in a sidebar that may be behind another window or
        another desk. The tab always shows what needs you, and that layer is never the only record — the sidebar list is — so nothing can
        be missed permanently.
      </p>
      <p>
        Notifications stay short and dull on purpose, because they land on screens other people can see: the repository and issue number
        only, never the issue title, the question a colony asked, or an error.
      </p>
    </>
  );

  // A permission the user revoked in the browser keeps the stored switch honest: it cannot count as on.
  const browserOn = prefs.browser && permission === "granted";

  return (
    <Pane title="Notifications" subtitle="How a colony that needs you gets your attention" info={info} back={back}>
      <div className="space-y-4">
        <Row
          id="notifications-in-tab"
          label="In this tab"
          help="Shows how many colonies need you in the tab title, a dot on the favicon, and a strip above the colony list."
          inline
        >
          <Switch
            id="notifications-in-tab"
            labelledBy="notifications-in-tab-label"
            label="In this tab"
            checked={prefs.inTab}
            onChange={(checked) => patch({ inTab: checked })}
          />
        </Row>
        <Row id="notifications-sound" label="Play a sound when a colony asks a question" help="A short chime, so you notice even when you are looking away." inline>
          <Switch
            id="notifications-sound"
            labelledBy="notifications-sound-label"
            label="Play a sound when a colony asks a question"
            checked={prefs.sound}
            onChange={(checked) => patch({ sound: checked })}
          />
        </Row>
        <Row id="notifications-browser" label="Browser notifications while the tab is not in front" help="Your browser shows a desktop notification. It asks your permission the first time." inline>
          <Switch
            id="notifications-browser"
            labelledBy="notifications-browser-label"
            label="Browser notifications while the tab is not in front"
            checked={browserOn}
            disabled={permission === "unsupported"}
            onChange={setBrowser}
          />
        </Row>
        {permission === "unsupported" && (
          <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
            This browser does not offer notifications — the API is missing, or the page is not on a secure origin.
          </p>
        )}
        {permission === "denied" && (
          <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
            The browser is blocking notifications for this site. Allow Colonizer in the browser’s own site settings, then switch this on
            here — this switch cannot lift a block the browser set.
          </p>
        )}
        {asked && permission === "default" && !prefs.browser && (
          <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
            The permission prompt was dismissed without an answer. Switch it on again to ask once more.
          </p>
        )}

        <div>
          <h4 className="mb-1 text-small-lg font-semibold">Push to this device</h4>
          <p className="mb-1 text-small-lg text-muted">
            Web push reaches this browser with Colonizer closed — a phone that never has the tab open. A notification tap opens the colony it
            names. The same short-and-dull rules apply as above.
          </p>
          <Row
            id="notifications-push"
            label="Push to this device"
            info={
              <p>
                Subscribes this browser through the mothership's push key and lists it below. Revoking a row stops that device's pushes; the
                browser's own registration is dropped too when it is the one revoked.
              </p>
            }
            inline
          >
            <Button variant="secondary" disabled={!pushable || subscribing} onClick={enrolThisDevice}>
              {subscribing && <Spinner />}
              {subscribing ? "Subscribing…" : "Subscribe"}
            </Button>
          </Row>
          {!pushable && !showIosInstallHint() && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-small-lg text-muted">
              This browser cannot join web push. On iPhone and iPad it needs iOS 16.4 or newer with Colonizer added to the Home Screen;
              everywhere else it needs a secure origin and a browser with push support.
            </p>
          )}
          {!pushable && showIosInstallHint() && <IosHomeScreenSheet />}
          {subs !== null && subs.length > 0 && (
            <PushDeviceList
              subs={subs}
              orgs={orgs}
              ownIds={ownIds}
              onRevoke={revokeDevice}
              onChanged={(row) => setSubs((rows) => (rows ?? []).map((other) => (other.id === row.id ? row : other)))}
            />
          )}
        </div>

        <div>
          <h4 className="mb-1 text-small-lg font-semibold">Which events interrupt</h4>
          <p className="mb-1 text-small-lg text-muted">
            These gate the sound and the browser notifications. The tab title, the favicon and the strip always show every colony that needs
            you, whatever these say.
          </p>
          <Row id="notifications-event-question" label="A colony asks a question" inline>
            <Switch
              id="notifications-event-question"
              labelledBy="notifications-event-question-label"
              label="A colony asks a question"
              checked={prefs.events.question}
              onChange={(checked) => patch({ events: { ...prefs.events, question: checked } })}
            />
          </Row>
          <Row id="notifications-event-attention" label="A colony has stalled, or is out of nudges" inline>
            <Switch
              id="notifications-event-attention"
              labelledBy="notifications-event-attention-label"
              label="A colony has stalled, or is out of nudges"
              checked={prefs.events.attention}
              onChange={(checked) => patch({ events: { ...prefs.events, attention: checked } })}
            />
          </Row>
          <Row id="notifications-event-failed" label="A colony fails" inline>
            <Switch
              id="notifications-event-failed"
              labelledBy="notifications-event-failed-label"
              label="A colony fails"
              checked={prefs.events.failed}
              onChange={(checked) => patch({ events: { ...prefs.events, failed: checked } })}
            />
          </Row>
          <Row id="notifications-event-pull-request" label="A colony opens a pull request" inline>
            <Switch
              id="notifications-event-pull-request"
              labelledBy="notifications-event-pull-request-label"
              label="A colony opens a pull request"
              checked={prefs.events.pull_request}
              onChange={(checked) => patch({ events: { ...prefs.events, pull_request: checked } })}
            />
          </Row>
        </div>
      </div>
    </Pane>
  );
}
