// The pure half of the "Your cockpit" feature (issue #867): the addresses offered and their order,
// that nothing shown/copied/QR-ed can carry a token, query or hash, the per-device prompt memory,
// and the install-platform table. No DOM: every input is passed in.
import { describe, expect, it } from "vitest";

import {
  bookmarkShortcut,
  bookmarkState,
  bookmarkable,
  clearBookmark,
  cockpitAddresses,
  dismissBookmark,
  installPlatform,
  isLoopback,
  markBookmarkInstalled,
  networkGap,
  shouldOfferBookmark,
} from "./cockpitAddress";
import type { PhoneOrigin } from "./types";

const relayHost = "h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev";
const relayUrl = `https://${relayHost}`;
const relayOn: PhoneOrigin = { kind: "relay", url: relayUrl, reachable: true, secure: true, note: null };
const tailnet: PhoneOrigin = { kind: "tailnet", url: "http://100.72.1.4:7878", reachable: true, secure: false, note: null };
const tailnetDown: PhoneOrigin = { ...tailnet, reachable: false };
const lan: PhoneOrigin = { kind: "lan", url: "http://192.168.1.20:7878", reachable: true, secure: false, note: null };
const remoteOn = { enabled: true, host: relayHost };
const remoteOff = { enabled: false, host: relayHost };

describe("bookmarkable", () => {
  it("keeps scheme, host, port and base path only", () => {
    expect(bookmarkable("http://127.0.0.1:7878/")).toBe("http://127.0.0.1:7878");
    expect(bookmarkable("http://127.0.0.1:7878/?token=secret#code")).toBe("http://127.0.0.1:7878");
    expect(bookmarkable(`${relayUrl}/?pair=a1b2#x`)).toBe(relayUrl);
    expect(bookmarkable("https://example.com/cockpit/")).toBe("https://example.com/cockpit");
  });

  it("strips a query and hash even from something it cannot parse", () => {
    expect(bookmarkable("not a url?token=x#y")).toBe("not a url");
  });

  it("is not fooled into a loopback by a host that merely contains one", () => {
    expect(isLoopback("http://localhost:7878")).toBe(true);
    expect(isLoopback("http://127.0.0.1:7878")).toBe(true);
    expect(isLoopback("http://[::1]:7878")).toBe(true);
    expect(isLoopback("http://192.168.1.20:7878")).toBe(false);
    expect(isLoopback("https://localhost.evil.test")).toBe(false);
  });
});

describe("cockpitAddresses", () => {
  it("offers this computer when the page is on the loopback", () => {
    const list = cockpitAddresses({ here: "http://127.0.0.1:7878/?view=home", remote: remoteOff });
    expect(list).toHaveLength(1);
    expect(list[0]).toEqual({ kind: "local", label: "On this computer", url: "http://127.0.0.1:7878" });
  });

  it("picks the network origin the same way the pairing QR does, tailnet before lan", () => {
    const list = cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [relayOn, tailnet, lan], remote: remoteOff });
    const network = list.find((a) => a.kind === "network");
    expect(network?.url).toBe(tailnet.url);
    expect(network?.label).toBe("On your tailnet");
  });

  it("prefers a reachable origin over an earlier unreachable one", () => {
    const list = cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [tailnetDown, lan], remote: remoteOff });
    expect(list.find((a) => a.kind === "network")?.url).toBe(lan.url);
  });

  it("lists the remote link once, as anywhere, and never the relay origin as a network address", () => {
    const list = cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [relayOn], remote: remoteOn });
    expect(list.map((a) => a.kind)).toEqual(["local", "anywhere"]);
    expect(list.filter((a) => a.url === relayUrl)).toHaveLength(1);
  });

  it("only offers anywhere while remote access is on with a host", () => {
    expect(cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [relayOn], remote: remoteOff }).some((a) => a.kind === "anywhere")).toBe(false);
    expect(cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [], remote: { enabled: true, host: null } }).some((a) => a.kind === "anywhere")).toBe(false);
  });

  it("offers this page's own address as the network one when it is not loopback", () => {
    const list = cockpitAddresses({ here: "http://100.72.1.4:7878/", origins: [], remote: remoteOff });
    expect(list).toEqual([{ kind: "network", label: "On your network", url: "http://100.72.1.4:7878" }]);
  });

  it("never lists the address this page is open on when that is the remote link", () => {
    const list = cockpitAddresses({ here: `${relayUrl}/`, origins: [], remote: remoteOn });
    expect(list).toEqual([{ kind: "anywhere", label: "Anywhere", url: relayUrl }]);
  });

  it("gives no shown, copied or QR-encoded url a token, query or hash", () => {
    const dirty: PhoneOrigin = { kind: "tailnet", url: "http://100.72.1.4:7878/?token=secret#code", reachable: true, secure: false, note: null };
    const list = cockpitAddresses({
      here: "http://127.0.0.1:7878/?token=abc#code",
      origins: [{ ...relayOn, url: `${relayUrl}/?token=x#y` }, dirty],
      remote: remoteOn,
    });
    for (const a of list) {
      expect(a.url).not.toMatch(/[?#]/);
      expect(a.url).not.toContain("token=");
      expect(a.url).not.toContain("secret");
    }
  });

  it("leaves an unreachable network origin out, and explains the gap", () => {
    const bindNote = "The mothership listens on 127.0.0.1:7878 only; set COLONIZER_BIND=0.0.0.0:7878";
    const down: PhoneOrigin = { kind: "lan", url: "http://192.168.1.20:7878", reachable: false, secure: false, note: bindNote };
    const list = cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [down], remote: remoteOff });
    expect(list.map((a) => a.kind)).toEqual(["local"]); // a dead link is not an address
    const gap = networkGap(list, [down]);
    expect(gap).toContain("Not reachable from other devices");
    expect(gap).toContain(bindNote);
    expect(gap).toContain("Settings → Remote access");
  });

  it("warns under a reachable plain-http network origin, with its note or its own wording", () => {
    const note = "Plain http: prefer the relay link";
    const plain: PhoneOrigin = { kind: "lan", url: "http://192.168.1.20:7878", reachable: true, secure: false, note };
    expect(cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [plain], remote: remoteOff }).find((a) => a.kind === "network")?.note).toBe(note);
    expect(cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [tailnet], remote: remoteOff }).find((a) => a.kind === "network")?.note).toMatch(/Plain http/);
  });

  it("has no gap once something is reachable from another device", () => {
    expect(networkGap(cockpitAddresses({ here: "http://127.0.0.1:7878", origins: [relayOn], remote: remoteOn }), [relayOn])).toBeNull();
    expect(networkGap(cockpitAddresses({ here: "http://100.72.1.4:7878/", origins: [], remote: remoteOff }), [])).toBeNull();
  });
});

describe("bookmark prompt memory", () => {
  const fakeStore = () => {
    const map: Record<string, string> = {};
    return {
      get: (key: string) => map[key] ?? null,
      set: (key: string, value: string | null) => {
        if (value === null) delete map[key];
        else map[key] = value;
      },
    };
  };

  it("dismiss remembers a no, and the prompt never offers again", () => {
    const s = fakeStore();
    expect(bookmarkState(s.get)).toBeNull();
    expect(shouldOfferBookmark({ state: bookmarkState(s.get), standalone: false })).toBe(true);
    dismissBookmark(s.set);
    expect(bookmarkState(s.get)).toBe("dismissed");
    expect(shouldOfferBookmark({ state: bookmarkState(s.get), standalone: false })).toBe(false);
  });

  it("installed and standalone both stand the prompt down", () => {
    const s = fakeStore();
    markBookmarkInstalled(s.set);
    expect(shouldOfferBookmark({ state: bookmarkState(s.get), standalone: false })).toBe(false);
    expect(shouldOfferBookmark({ state: null, standalone: true })).toBe(false);
    expect(shouldOfferBookmark({ state: null, standalone: false })).toBe(true);
  });

  it("clear forgets the answer, so the card can re-offer", () => {
    const s = fakeStore();
    dismissBookmark(s.set);
    clearBookmark(s.set);
    expect(bookmarkState(s.get)).toBeNull();
  });

  it("ignores a value it did not write", () => {
    expect(bookmarkState(() => "something-else")).toBeNull();
  });
});

describe("bookmarkShortcut", () => {
  it("is ⌘D on the Mac and Ctrl+D everywhere else", () => {
    expect(bookmarkShortcut("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)")).toBe("⌘D");
    expect(bookmarkShortcut("Mozilla/5.0 (X11; Linux x86_64)")).toBe("Ctrl+D");
    expect(bookmarkShortcut("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")).toBe("Ctrl+D");
  });
});

describe("installPlatform", () => {
  const iphoneSafari = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
  const iphoneChrome = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/120.0.6099.119 Mobile/15E148 Safari/604.1";
  const instagram = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 Instagram 320.0.0.1";
  const androidChrome = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";
  const androidSamsung = "Mozilla/5.0 (Linux; Android 14; SM-S918B) AppleWebKit/537.36 (KHTML, like Gecko) SamsungBrowser/23.0 Chrome/115.0.0.0 Mobile Safari/537.36";
  const desktopChrome = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

  it("maps each browser to the story to tell it", () => {
    expect(installPlatform(iphoneSafari, false, 5)).toBe("ios-safari");
    expect(installPlatform(iphoneChrome, false, 5)).toBe("ios-other");
    expect(installPlatform(instagram, false, 5)).toBe("ios-other");
    expect(installPlatform(iphoneSafari, false, 5, true)).toBe("ios-other"); // Brave: Safari's agent + navigator.brave
    expect(installPlatform(iphoneSafari, false, 5, false)).toBe("ios-safari");
    expect(installPlatform(androidChrome, false, 0)).toBe("android-chrome");
    expect(installPlatform(androidSamsung, false, 0)).toBe("android-other");
    expect(installPlatform(desktopChrome, false, 0)).toBe("desktop");
  });

  it("reports installed first, whatever the browser", () => {
    expect(installPlatform(iphoneSafari, true, 5)).toBe("installed");
    expect(installPlatform(iphoneChrome, true, 5, true)).toBe("installed");
    expect(installPlatform(desktopChrome, true, 0)).toBe("installed");
  });

  it("treats an iPad posing as a Macintosh as iOS", () => {
    const ipad = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
    expect(installPlatform(ipad, false, 5)).toBe("ios-safari");
    expect(installPlatform(ipad, false, 0)).toBe("desktop");
  });
});
