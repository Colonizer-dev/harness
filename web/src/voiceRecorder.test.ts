import { describe, expect, it } from "vitest";

import { countdown, keySourceLabel, pickFormat } from "./voiceRecorder";

describe("pickFormat", () => {
  it("prefers Opus in WebM, then MP4 for Safari, else lets the browser choose", () => {
    expect(pickFormat(() => true)).toBe("audio/webm;codecs=opus");
    expect(pickFormat((t) => t === "audio/mp4")).toBe("audio/mp4");
    expect(pickFormat(() => false)).toBe("");
  });
});

describe("countdown", () => {
  it("speaks up only over the last ten seconds, and never below zero", () => {
    expect(countdown(0, 120)).toBeNull();
    expect(countdown(109_500, 120)).toBeNull();
    expect(countdown(110_000, 120)).toBe("10s");
    expect(countdown(119_200, 120)).toBe("1s");
    expect(countdown(125_000, 120)).toBe("0s");
  });
});

describe("keySourceLabel", () => {
  it("says where a key comes from without ever showing it", () => {
    expect(keySourceLabel("saved", false)).toBe("Saved on this machine.");
    expect(keySourceLabel("OPENAI_API_KEY", false)).toBe("Read from OPENAI_API_KEY.");
    expect(keySourceLabel("provider:openai-main", false)).toBe("Reused from the openai-main model provider.");
    expect(keySourceLabel(null, false)).toBe("Not set.");
    expect(keySourceLabel(null, true)).toContain("may not need one");
  });
});
