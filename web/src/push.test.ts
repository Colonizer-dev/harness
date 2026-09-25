// The pure half of the web-push client (issue #516): the key decoding the subscription needs, the
// label a device enrols under, and the colony a push's url names. The browser-touching half of
// push.ts is untested, like notifications.ts's lower half.
import { describe, expect, it } from "vitest";

import { colonyFromUrl, deviceLabel, urlBase64ToUint8Array } from "./push";

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
