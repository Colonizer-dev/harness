// Settings → Add your phone (issue #746), rendered to static markup like the cockpit's other tests:
// which origin the QR names, that the QR carries an invite and never a credential, what each warning
// state explains, and the paired phones with their Revoke. The QR itself is RemoteAccessPane's.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { PhoneInvite, PhoneOrigin, Phones } from "../types";
import { PhonePane, chosenOrigin, inviteExpiry, inviteUrl } from "./PhonePane";

const api = { phones: () => new Promise(() => {}) } as unknown as Api;
const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={api}>{node}</ApiContext.Provider>);

const relayUrl = "https://h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev";
const relayOn: PhoneOrigin = { kind: "relay", url: relayUrl, reachable: true, secure: true, note: null };
const relayDown: PhoneOrigin = { ...relayOn, reachable: false, note: "Remote access is on, but its link is not connected right now" };
const tailnet: PhoneOrigin = { kind: "tailnet", url: "http://100.72.1.4:7878", reachable: true, secure: false, note: "Plain http" };
const lan: PhoneOrigin = { kind: "lan", url: "http://192.168.1.20:7878", reachable: true, secure: false, note: "Plain http" };
const lanClosed: PhoneOrigin = { ...lan, reachable: false, note: "The mothership listens on 127.0.0.1:7878 only; set COLONIZER_BIND=0.0.0.0:7878" };

const invite = (origins: PhoneOrigin[]): PhoneInvite => ({
  code: "a1b2c3d4e5f6",
  expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
  ttl_secs: 300,
  origins,
});
const pane = (i: PhoneInvite | null, phones: Phones | null = null) => wrap(<PhonePane initialInvite={i} initialPhones={phones} />);

describe("chosenOrigin", () => {
  it("takes the first reachable origin, in the mothership's preference order", () => {
    expect(chosenOrigin([relayOn, tailnet, lan])?.kind).toBe("relay");
    expect(chosenOrigin([relayDown, tailnet, lan])?.kind).toBe("tailnet");
    expect(chosenOrigin([lan])?.kind).toBe("lan");
  });

  it("falls back to the first origin at all, so the pane can still explain itself", () => {
    expect(chosenOrigin([relayDown, lanClosed])?.kind).toBe("relay");
    expect(chosenOrigin([])).toBeNull();
  });
});

describe("inviteUrl and inviteExpiry", () => {
  it("rides the invite in the query, on the origin's base", () => {
    expect(inviteUrl(relayOn, "a1b2")).toBe(`${relayUrl}/?pair=a1b2`);
    expect(inviteUrl(lan, "a b&c")).toBe("http://192.168.1.20:7878/?pair=a%20b%26c");
  });

  it("reads the RFC3339 stamp, and passes junk through", () => {
    const now = Date.parse("2026-09-29T12:00:00Z");
    expect(inviteExpiry({ expires_at: "2026-09-29T12:05:00Z" }, now)).toBe("expires in 5 min");
    expect(inviteExpiry({ expires_at: "not a stamp" }, now)).toBe("not a stamp");
  });
});

describe("PhonePane", () => {
  it("starts as an explainer and a button, with no code anywhere", () => {
    const html = pane(null);
    expect(html).toContain("Show a code to scan");
    expect(html).toContain("six-digit code");
    expect(html).toContain("No phone is paired yet.");
    expect(html).not.toContain("pair=");
  });

  it("shows the relay QR with the invite, and the box for the phone's code", () => {
    const html = pane(invite([relayOn, lan]));
    expect(html).toContain(`aria-label="QR code for ${relayUrl}/?pair=a1b2c3d4e5f6"`);
    expect(html).toContain("Relay link");
    expect(html).toContain('aria-label="Code shown on your phone"');
    expect(html).toContain("expires in 5 min");
    expect(html).toContain("single use");
    expect(html).toContain("installing the app again");
    expect(html).not.toContain("plain http"); // a secure origin needs no warning
    expect(html).not.toContain("token");
  });

  it("names the tailnet when that is the best there is, and says it is plain http", () => {
    const html = pane(invite([tailnet, lan]));
    expect(html).toContain("Tailnet");
    expect(html).toContain("100.72.1.4:7878/?pair=");
    expect(html).toContain("plain http and only works on your tailnet");
  });

  it("warns that a lan origin only works at home", () => {
    const html = pane(invite([lan]));
    expect(html).toContain("Your network");
    expect(html).toContain("plain http and only works on your network");
  });

  it("explains itself, with the server's notes, when nothing is reachable", () => {
    const html = pane(invite([relayDown, lanClosed]));
    expect(html).toContain("can&#x27;t reach this machine from outside");
    expect(html).toContain("not connected right now");
    expect(html).toContain("COLONIZER_BIND=0.0.0.0:7878");
    expect(html).toContain("Settings → Remote access");
  });

  it("lists paired phones with a Revoke each, and phones waiting for their code", () => {
    const html = pane(null, {
      devices: [
        { id: "dev_1", label: "iPhone", paired_at: new Date().toISOString() },
        { id: "dev_2", label: "Android phone", paired_at: new Date().toISOString() },
      ],
      pending: [{ id: "ph_1", label: "iPad", expires_at: new Date(Date.now() + 60_000).toISOString() }],
    });
    expect(html).toContain("iPhone");
    expect(html).toContain("Android phone");
    expect(html.match(/>Revoke</g)).toHaveLength(2);
    expect(html).toContain("Waiting for their code");
    expect(html).toContain("Turn down");
    expect(html).not.toContain("No phone is paired yet.");
  });
});
