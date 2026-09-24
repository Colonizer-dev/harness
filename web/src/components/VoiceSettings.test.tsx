import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { VoiceKeyRow, VoiceTestRow } from "./SettingsDialog";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={{} as Api}>{node}</ApiContext.Provider>);

describe("Voice settings", () => {
  it("offers a write-only key field named for the service", () => {
    const html = wrap(<VoiceKeyRow provider="groq" name="Groq" />);
    expect(html).toContain("Groq API key");
    expect(html).toContain('type="password"');
    expect(html).toContain("never the key");
    expect(html).toContain("Checking…");
  });

  it("asks for a save before testing an unsaved choice", () => {
    expect(wrap(<VoiceTestRow unsaved />)).toContain("Save first: the test uses the saved service.");
    expect(wrap(<VoiceTestRow unsaved={false} />)).toContain("Test microphone");
  });
});
