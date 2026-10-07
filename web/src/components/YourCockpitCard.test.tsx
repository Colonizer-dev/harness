// The Your cockpit card, rendered to static markup: the addresses it lists (loopback and the remote
// link), that Copy and Show QR are offered, and — the point — that nothing it renders carries the
// one-time sign-in token from the page it was opened on.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { PhoneOrigin, RemoteStatus } from "../types";
import { YourCockpitCard } from "./YourCockpitCard";

const api = { remote: () => new Promise<RemoteStatus>(() => {}), phones: () => new Promise(() => {}) } as unknown as Api;
const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={api}>{node}</ApiContext.Provider>);

const remoteOn: RemoteStatus = { enabled: true, host: "h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev", connected: true, since: null, replaced: false, require_github: false };
const tailnet: PhoneOrigin = { kind: "tailnet", url: "http://100.72.1.4:7878", reachable: true, secure: false, note: null };

describe("YourCockpitCard", () => {
  it("lists this computer and anywhere, with Copy and Show QR", () => {
    const html = wrap(<YourCockpitCard here="http://127.0.0.1:7878/" remote={remoteOn} onOpenPhone={() => {}} />);
    expect(html).toContain("On this computer");
    expect(html).toContain("Anywhere");
    expect(html).toContain("http://127.0.0.1:7878");
    expect(html).toContain("https://h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev");
    expect(html).toContain('aria-label="On this computer address"');
    expect(html).toContain(">Copy<");
    expect(html).toContain("Show QR");
    expect(html).toContain("Put it on your phone");
    expect(html).toContain("Add to this device");
  });

  it("never shows the one-time sign-in token or a pairing code it was opened with", () => {
    const html = wrap(<YourCockpitCard here="http://127.0.0.1:7878/?token=secret-token#pair=abc" remote={remoteOn} />);
    expect(html).not.toContain("secret-token");
    expect(html).not.toContain("#pair");
    expect(html).toContain("http://127.0.0.1:7878");
  });

  it("offers no anywhere when remote access is off", () => {
    const html = wrap(<YourCockpitCard here="http://127.0.0.1:7878/" remote={{ ...remoteOn, enabled: false }} />);
    expect(html).not.toContain("Anywhere");
    expect(html).toContain("On this computer");
  });

  it("names the network address from the server's origins, not just the page it is open on", () => {
    const html = wrap(<YourCockpitCard here="http://127.0.0.1:7878/" remote={remoteOn} origins={[tailnet]} />);
    expect(html).toContain("On your tailnet");
    expect(html).toContain("http://100.72.1.4:7878");
    expect(html).not.toContain("?token=");
  });

  it("falls back to this page's address when an old server sends no origins", () => {
    const html = wrap(<YourCockpitCard here="http://192.168.1.20:7878/" remote={null} />);
    expect(html).toContain("On your network");
    expect(html).toContain("http://192.168.1.20:7878");
  });

  it("leaves an unreachable network origin out and says the cockpit is not reachable", () => {
    const down: PhoneOrigin = { kind: "lan", url: "http://192.168.1.20:7878", reachable: false, secure: false, note: "The mothership listens on 127.0.0.1:7878 only" };
    const html = wrap(<YourCockpitCard here="http://127.0.0.1:7878/" remote={null} origins={[down]} />);
    expect(html).not.toContain("On your network");
    expect(html).not.toContain("192.168.1.20");
    expect(html).toContain("Not reachable from other devices");
    expect(html).toContain("127.0.0.1:7878 only");
  });

  it("warns under a reachable plain-http network address", () => {
    const plain: PhoneOrigin = { kind: "lan", url: "http://192.168.1.20:7878", reachable: true, secure: false, note: "Plain http: prefer the relay link" };
    const html = wrap(<YourCockpitCard here="http://127.0.0.1:7878/" remote={null} origins={[plain]} />);
    expect(html).toContain("On your network");
    expect(html).toContain("Plain http: prefer the relay link");
  });
});
