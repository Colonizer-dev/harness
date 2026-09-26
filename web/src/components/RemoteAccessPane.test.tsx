// Settings → Remote access (issue #535), rendered to static markup like the cockpit's other tests:
// what each state of the switch shows. The interactive paths (copy, confirm, reset) live in their
// handlers, so the pure helpers behind the markup are pinned directly.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import type { RemotePairing, RemoteStatus } from "../types";
import { QrCode, RemoteAccessPane, connectionText, expiryText, formatPairingCode, remoteLink } from "./RemoteAccessPane";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={{} as Api}>{node}</ApiContext.Provider>);

const off: RemoteStatus = { enabled: false, host: null, connected: false, since: null };
const onConnected: RemoteStatus = { enabled: true, host: "h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev", connected: true, since: "2026-09-25T00:16:11+00:00" };
const onOffline: RemoteStatus = { ...onConnected, connected: false, since: null };
const pendingPairing: RemotePairing = {
  owner: null,
  pending: [{ code: "123456", github_login: "octocat", expires_at: Math.floor(Date.now() / 1000) + 7 * 60 }],
};

const pane = (remote: RemoteStatus | null, initialPairing?: RemotePairing | null) =>
  wrap(<RemoteAccessPane remote={remote} onChanged={() => {}} initialPairing={initialPairing} />);

describe("RemoteAccessPane", () => {
  it("explains what it does and what it does not expose while off, and shows no link", () => {
    const html = pane(off);
    expect(html).toContain("Remote access");
    expect(html).toContain("Allow remote access"); // the switch row, distinct from the pane title
    expect(html).toContain("dials out an encrypted tunnel");
    expect(html).toContain("nothing else on this machine");
    expect(html).toContain("access token"); // the cockpit's own sign-in still applies behind the link
    expect(html).toContain("public key"); // what the relay does store
    expect(html).toContain('aria-checked="false"');
    expect(html).not.toContain("https://");
  });

  it("shows the link, Copy and QR buttons, the connected status and the pending code while on", () => {
    const html = pane(onConnected, pendingPairing);
    expect(html).toContain('aria-checked="true"');
    // Displayed with a <wbr> after every dot so a narrow box wraps on them; Copy and the QR keep the plain link.
    expect(html).toContain("https://h4xk2q7mzt5pw3nd6vrc.<wbr/>my.<wbr/>colonizer.<wbr/>dev");
    expect(html).toContain(">Copy</button>");
    expect(html).toContain(">QR code</button>");
    expect(html).toContain("Connected since");
    expect(html).not.toContain("Offline");
    // The pending request: the code in its two groups, the login, and the warning that gates Confirm.
    expect(html).toContain("123 456");
    expect(html).toContain("@octocat");
    expect(html).toContain("expires in 7 min");
    expect(html).toContain("Confirm");
    expect(html).toContain("same code");
  });

  it("says the tunnel is offline — reconnecting while enabled but not connected", () => {
    expect(pane(onOffline)).toContain("Offline — reconnecting");
  });

  it("shows the owner once paired, and hides pairing entirely without an endpoint", () => {
    const paired = pane(onConnected, { owner: { github_login: "octocat" }, pending: [] });
    expect(paired).toContain("Paired with @octocat");
    expect(paired).not.toContain(">Confirm</button>");
    expect(pane(onConnected, null)).not.toContain("Pairing");
  });

  it("keeps the reset behind its two-step confirm", () => {
    expect(pane(onConnected)).toContain(">Reset link</button>");
    expect(pane(onConnected)).not.toContain("Really reset?");
  });
});

describe("QrCode", () => {
  it("draws the link as one accessible inline SVG path", () => {
    const html = wrap(<QrCode text="https://h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev" />);
    expect(html).toContain('role="img"');
    expect(html).toContain('aria-label="QR code for https://h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev"');
    expect(html).toContain("<path");
    expect(html).toContain("<svg");
  });
});

describe("remote helpers", () => {
  it("builds the link from the host, and nothing before the first enable", () => {
    expect(remoteLink(onConnected)).toBe("https://h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev");
    expect(remoteLink(off)).toBeNull();
  });

  it("splits a six-digit code into the two groups a phone shows", () => {
    expect(formatPairingCode("123456")).toBe("123 456");
    expect(formatPairingCode("12345")).toBe("12345"); // anything else passes through
  });

  it("names the connection state", () => {
    expect(connectionText(onConnected)).toMatch(/^Connected since /);
    expect(connectionText(onOffline)).toBe("Offline — reconnecting");
    expect(connectionText({ ...onConnected, since: null })).toBe("Offline — reconnecting");
  });

  it("reads a pairing expiry in unix seconds", () => {
    const now = Date.parse("2026-09-25T00:00:00Z");
    expect(expiryText(Math.floor(now / 1000) + 7 * 60, now)).toBe("expires in 7 min");
    expect(expiryText(Math.floor(now / 1000) + 90 * 60, now)).toMatch(/^expires at /);
    expect(expiryText(Math.floor(now / 1000) - 60, now)).toBe("expires now");
  });
});
