// The pure half of the web-push client (issue #516): the key decoding the subscription needs, the
// label a device enrols under, the colony a push's url names, and the per-device prefs' defaults,
// quiet-hours arithmetic and repo-filter shape (issue #743). The browser-touching half of push.ts
// is untested, like notifications.ts's lower half.
import { describe, expect, it } from "vitest";

import { colonyFromUrl, defaultPushPrefs, deviceLabel, mergePushPrefs, minutesToTime, tabFocused, timeToMinutes, urlBase64ToUint8Array, validScopeEntry } from "./push";

describe("colonyFromUrl", () => {
  it("names the colony a push payload or the url bar points at", () => {
    expect(colonyFromUrl("/?colony=demo1234")).toBe("demo1234");
    expect(colonyFromUrl("https://cockpit.test/?colony=abc123&x=1")).toBe("abc123");
    expect(colonyFromUrl("https://cockpit.test/?x=1&colony=second")).toBe("second");
  });

  it("comes up with nothing when the url carries no colony", () => {
    expect(colonyFromUrl("/")).toBeNull();
    expect(colonyFromUrl("https://cockpit.test/settings")).toBeNull();
    expect(colonyFromUrl("/?colony=")).toBeNull();
    expect(colonyFromUrl("not a url")).toBeNull();
  });
});

describe("urlBase64ToUint8Array", () => {
  it("decodes base64url, padding and all, to the bytes the VAPID key arrived as", () => {
    // "hello" → base64url "aGVsbG8" (no padding), base64 "aGVsbG8=".
    expect([...urlBase64ToUint8Array("aGVsbG8")]).toEqual([104, 101, 108, 108, 111]);
    // Both url-safe letters map back onto the standard alphabet: "-_" is "+/" is 0xfb.
    expect([...urlBase64ToUint8Array("-_")]).toEqual([251]);
  });

  it("round-trips a P-256-sized key to a 65-byte application server key", () => {
    // A realistic 65-byte uncompressed P-256 point, base64url (the 0x04 prefix first).
    const key = "BB5fVboJOnLBVPursGoy1AZA5DXhRqSdoaBnAGjI8NeR1PuBgnN3Vx6rbF5pvoxqTOhaLHQwxrRLmZgA2pHcg0k";
    expect(urlBase64ToUint8Array(key)).toHaveLength(65);
    expect(urlBase64ToUint8Array(key)[0]).toBe(4);
  });
});

describe("deviceLabel", () => {
  it("names the phone or computer first, then the browser", () => {
    expect(deviceLabel("Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1")).toBe("iPhone · Safari");
    expect(deviceLabel("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Mobile Safari/537.36")).toBe("Android · Chrome");
    expect(deviceLabel("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")).toBe("Mac · Chrome");
    expect(deviceLabel("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0")).toBe("Windows · Edge");
    expect(deviceLabel("Mozilla/5.0 (X11; Linux x86_64; rv:125.0) Gecko/20100101 Firefox/125.0")).toBe("Linux · Firefox");
  });

  it("still names the device when the browser is unrecognised", () => {
    expect(deviceLabel("Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X)")).toBe("iPhone");
    expect(deviceLabel("")).toBe("Device");
  });
});

describe("push prefs defaults", () => {
  it("start loud: the colony-needs-you events on, the two quiet kinds off", () => {
    expect(defaultPushPrefs().events).toEqual({
      question: true,
      pull_request: true,
      needs_rebase: true,
      failed: true,
      attention: true,
      provider_degraded: false,
      digest: false,
    });
    expect(defaultPushPrefs().question_sound).toBe(true);
    // The #742 answer buttons and the #744 badge start on, like the mothership's Prefs::default.
    expect(defaultPushPrefs().answer_actions).toBe(true);
    expect(defaultPushPrefs().badge).toBe(true);
    expect(defaultPushPrefs().quiet).toBeNull();
  });

  it("fills every gap a stored row leaves, key by key", () => {
    expect(mergePushPrefs(null)).toEqual(defaultPushPrefs());
    expect(mergePushPrefs({})).toEqual(defaultPushPrefs());
    expect(mergePushPrefs({ answer_actions: false }).answer_actions).toBe(false);
    const stored = defaultPushPrefs();
    expect(mergePushPrefs({ events: { question: false }, quiet: { start: 1320, end: 420 }, scope: ["acme"] })).toEqual({
      ...stored,
      events: { ...stored.events, question: false },
      quiet: { start: 1320, end: 420 },
      scope: ["acme"],
    });
  });
});

describe("quiet-hours minute arithmetic", () => {
  it("minutes to the HH:MM a time input shows", () => {
    expect(minutesToTime(0)).toBe("00:00");
    expect(minutesToTime(450)).toBe("07:30");
    expect(minutesToTime(1320)).toBe("22:00");
    // Out of range clamps instead of producing a time the input would reject.
    expect(minutesToTime(-5)).toBe("00:00");
    expect(minutesToTime(1500)).toBe("23:59");
  });

  it("HH:MM back to minutes, null for anything an emptied input leaves", () => {
    expect(timeToMinutes("22:00")).toBe(1320);
    expect(timeToMinutes("7:30")).toBe(450);
    expect(timeToMinutes(" 07:30 ")).toBe(450);
    expect(timeToMinutes("")).toBeNull();
    expect(timeToMinutes("24:00")).toBeNull();
    expect(timeToMinutes("10:60")).toBeNull();
    expect(timeToMinutes("night")).toBeNull();
  });
});

describe("scope entries", () => {
  it("accepts an org or an org/repo, in the mothership's repo shape", () => {
    expect(validScopeEntry("acme")).toBe(true);
    expect(validScopeEntry("acme/webshop")).toBe(true);
    expect(validScopeEntry("ACME/web-shop_2.x")).toBe(true);
  });

  it("refuses anything else a textarea might grow", () => {
    expect(validScopeEntry("")).toBe(false);
    expect(validScopeEntry("acme/")).toBe(false);
    expect(validScopeEntry("acme/webshop/extra")).toBe(false);
    expect(validScopeEntry("acme/web shop")).toBe(false);
    expect(validScopeEntry(".")).toBe(false);
    expect(validScopeEntry("acme/..")).toBe(false);
  });
});

describe("tabFocused", () => {
  it("takes both visible and focused, like the browser reports them", () => {
    expect(tabFocused("visible", true)).toBe(true);
    expect(tabFocused("hidden", true)).toBe(false);
    expect(tabFocused("visible", false)).toBe(false);
    expect(tabFocused("prerender", false)).toBe(false);
  });
});
