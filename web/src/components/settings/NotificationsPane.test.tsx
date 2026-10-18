// The Cratefield delivery section (issue #1085), rendered to static markup like the other
// settings tests: the switch, the state line each delivery state produces, and when the test
// button is offered. The pane around it only loads the state and passes the calls through.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { CratefieldDelivery, cratefieldStateText } from "./NotificationsPane";
import type { CratefieldPushState } from "../../types";

const state = (over: Partial<CratefieldPushState>): CratefieldPushState => ({
  enabled: false,
  since: null,
  state: "off",
  queued: 0,
  dropped: 0,
  last_error: null,
  last_delivered: null,
  last_attempt: null,
  ...over,
});

const section = (cratefield: CratefieldPushState | null, loadFailed = false, busy = false) =>
  renderToStaticMarkup(<CratefieldDelivery state={cratefield} loadFailed={loadFailed} busy={busy} onSwitch={() => {}} onTest={() => {}} />);

describe("CratefieldDelivery", () => {
  it("names the channel, keeps the switch off and says where notifications stay", () => {
    const html = section(state({}));
    expect(html).toContain("Deliver through Cratefield");
    expect(html).toContain('id="notifications-cratefield"');
    expect(html).toContain('aria-checked="false"');
    expect(html).toContain("Notifications go only to the devices enrolled for web push");
    expect(html).toContain("Needs remote access"); // the help line: the relay calls are signed
    expect(html).not.toContain("dropped, for the cap"); // nothing dropped yet, so the count stays quiet
  });

  it("keeps the switch disabled while the state has not loaded, and says so when the load failed", () => {
    expect(section(null)).toContain("disabled");
    expect(section(null)).not.toContain("could not be loaded"); // still loading is not a failure
    expect(section(null, true)).toContain("The delivery state could not be loaded");
    expect(section(null, true)).toContain("it may well be on");
  });

  it("offers Send test only while enabled, and says what the last delivery did", () => {
    expect(section(state({}))).not.toContain("Send test");
    const html = section(
      state({ enabled: true, state: "ok", last_delivered: new Date(Date.now() - 120_000).toISOString() }),
    );
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain(">Send test</button>");
    expect(html).toMatch(/Delivered\. The last batch went through \d+m ago\./);
  });

  it("counts the queue while a batch waits, and owns up to drops", () => {
    const html = section(state({ enabled: true, state: "queued", queued: 2 }));
    expect(html).toContain("2 notifications queued for the relay");
    expect(section(state({ enabled: true, state: "queued", queued: 1 }))).toContain("1 notification queued");
    const dropped = section(state({ enabled: true, state: "ok", dropped: 3 }));
    expect(dropped).toContain("3 dropped, for the cap"); // the apostrophe renders as &#x27;
  });

  it("says the relay refused the last delivery — in the relay's own words — and names remote access as the missing piece", () => {
    const html = section(state({ enabled: true, state: "unreachable", queued: 1, last_error: "the relay answered 503 Service Unavailable" }));
    expect(html).toContain("The relay did not take the last delivery");
    expect(html).toContain("the relay answered 503 Service Unavailable");
    expect(section(state({ enabled: true, state: "no_remote" }))).toContain("Waiting for remote access");
  });
});

describe("cratefieldStateText", () => {
  it("reads every state the server answers with", () => {
    expect(cratefieldStateText(state({}))).toMatch(/^Off\./);
    expect(cratefieldStateText(state({ enabled: true, state: "no_remote" }))).toMatch(/^Waiting for remote access/);
    expect(cratefieldStateText(state({ enabled: true, state: "queued", queued: 1 }))).toBe("1 notification queued for the relay.");
    expect(cratefieldStateText(state({ enabled: true, state: "unreachable" }))).toMatch(/^The relay did not take/);
    expect(cratefieldStateText(state({ enabled: true, state: "ok" }))).toBe("On. Nothing has been delivered yet.");
  });
});
