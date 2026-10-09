import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { MicButton, appendHeard } from "./voiceInput";

describe("appendHeard", () => {
  it("joins dictation onto what was typed with one space", () => {
    expect(appendHeard("", " fix the login bug ")).toBe("fix the login bug");
    expect(appendHeard("Fix the login bug  ", "and add a test")).toBe("Fix the login bug and add a test");
    expect(appendHeard("typed", "  ")).toBe("typed");
  });
});

describe("MicButton", () => {
  it("names the service it will use, and says when it is busy", () => {
    const idle = renderToStaticMarkup(<MicButton listening={false} label="Groq · whisper-large-v3-turbo" onClick={() => {}} />);
    expect(idle).toContain('aria-label="speak a task (Groq · whisper-large-v3-turbo)"');
    expect(idle).toContain('title="Speak — Groq · whisper-large-v3-turbo"');
    const busy = renderToStaticMarkup(<MicButton listening={false} busy label="Groq" onClick={() => {}} />);
    expect(busy).toContain('title="Transcribing…"');
    expect(busy).toContain("disabled");
    expect(renderToStaticMarkup(<MicButton listening label="x" onClick={() => {}} />)).toContain('aria-pressed="true"');
  });
});
