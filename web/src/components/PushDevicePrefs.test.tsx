// The push device list and its prefs editor (issue #743), rendered to static markup: the test
// environment has no DOM and no push, so the mock API stands in for the mothership.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { ReactNode } from "react";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import { defaultPushPrefs } from "../push";
import { PushDeviceEditor, PushDeviceList } from "./PushDevicePrefs";
import type { PushSubscriptionSummary } from "../types";

const row: PushSubscriptionSummary = {
  id: "push_test01",
  label: "Mac · Chrome",
  created_at: 1_757_000_000,
  endpoint_host: "fcm.googleapis.com",
  last_seen: null,
  prefs: { ...defaultPushPrefs(), scope: ["acme"], quiet: { start: 1320, end: 480 }, tz: "Europe/Berlin", utc_offset: 120 },
};

const render = (node: ReactNode) =>
  renderToStaticMarkup(<ApiContext.Provider value={createMockApi()}>{node}</ApiContext.Provider>);

describe("PushDeviceList", () => {
  it("shows each device with its last report and its three actions", () => {
    const html = render(<PushDeviceList subs={[row]} ownIds={new Set(["push_test01"])} onRevoke={() => {}} onChanged={() => {}} />);
    expect(html).toContain("Mac · Chrome");
    expect(html).toContain("fcm.googleapis.com");
    expect(html).toContain("last seen never");
    expect(html).toContain("Send test");
    expect(html).toContain("Revoke");
  });
});

describe("PushDeviceEditor", () => {
  it("renders every event switch, the repo filter and the quiet hours it stored", () => {
    const html = render(<PushDeviceEditor row={row} isThisDevice onSaved={() => {}} />);
    for (const label of ["Questions", "Pull request opened", "Needs rebase", "Failed", "Needs attention", "Provider degraded", "Hourly digest"]) {
      expect(html).toContain(label);
    }
    expect(html).toContain("Play a sound for questions");
    expect(html).toContain("Answer buttons on questions");
    expect(html).toContain("Needs-you count on the app icon");
    expect(html).toContain("acme");
    expect(html).toContain('value="22:00"');
    expect(html).toContain('value="08:00"');
    expect(html).toContain("Europe/Berlin");
    expect(html).toContain("Questions break through quiet hours");
  });

  it("shows a switch turned off as off", () => {
    const html = render(<PushDeviceEditor row={{ ...row, prefs: { ...row.prefs, answer_actions: false } }} isThisDevice onSaved={() => {}} />);
    const answer = html.slice(html.indexOf("Answer buttons on questions"));
    expect(answer).toMatch(/aria-checked="false"/);
  });

  it("keeps quiet hours folded away while they are off", () => {
    const html = render(
      <PushDeviceEditor
        row={{ ...row, prefs: { ...defaultPushPrefs(), tz: null, utc_offset: 0 } }}
        isThisDevice={false}
        onSaved={() => {}}
      />,
    );
    expect(html).not.toContain("Quiet from");
    expect(html).toContain("Questions break through quiet hours");
  });
});
