import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import type { Api } from "../../api";
import { ApiContext } from "../../context";
import { SpotlightDialog, isSpotlightKey, type KeyLike, type SpotlightHost } from "./Spotlight";

const key = (over: Partial<KeyLike> = {}): KeyLike => ({ key: "k", metaKey: true, ctrlKey: false, altKey: false, shiftKey: false, defaultPrevented: false, target: null, ...over });
const el = (tagName: string, closest: string | null = null) => ({ tagName, isContentEditable: false, closest: (sel: string) => (closest && sel.includes(closest) ? {} : null) }) as unknown as EventTarget;

describe("opening", () => {
  it("opens on ⌘K and Ctrl-K, and on / outside any field", () => {
    expect(isSpotlightKey(key())).toBe(true);
    expect(isSpotlightKey(key({ metaKey: false, ctrlKey: true, key: "K" }))).toBe(true);
    expect(isSpotlightKey(key({ metaKey: false, key: "/" }))).toBe(true);
    expect(isSpotlightKey(key({ metaKey: false, key: "/", target: el("DIV") }))).toBe(true);
  });
  it("leaves keys that belong to a field, the editor, a terminal or Settings alone", () => {
    expect(isSpotlightKey(key({ metaKey: false, key: "/", target: el("INPUT") }))).toBe(false);
    expect(isSpotlightKey(key({ metaKey: false, key: "/", target: el("TEXTAREA") }))).toBe(false);
    expect(isSpotlightKey(key({ target: el("DIV", ".monaco-editor") }))).toBe(false);
    expect(isSpotlightKey(key({ metaKey: false, key: "/" }), true)).toBe(false);
    expect(isSpotlightKey(key({ defaultPrevented: true }))).toBe(false);
    expect(isSpotlightKey(key({ altKey: true }))).toBe(false);
    expect(isSpotlightKey(key({ key: "j" }))).toBe(false);
  });
});

describe("the panel", () => {
  const host: SpotlightHost = {
    sessions: [],
    repos: [],
    orgs: [],
    org: "acme",
    view: "home",
    colony: null,
    updateAvailable: false,
    onNavigate: () => {},
    onOpenColony: () => {},
    onOpenSettings: () => {},
    onSelectOrg: () => {},
    onOpenRepo: () => {},
    onOpenChat: () => {},
  };
  it("renders an accessible combobox over a listbox with Colonize under Do", () => {
    const api = { chats: async () => ({ chats: [] }), loops: async () => [], modules: async () => [], issues: async () => [] } as unknown as Api;
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <SpotlightDialog host={host} onClose={() => {}} />
      </ApiContext.Provider>,
    );
    expect(html).toContain('role="dialog"');
    expect(html).toContain('role="combobox"');
    expect(html).toContain('role="listbox"');
    expect(html).toContain("Ask or search…");
    expect(html).toContain("Colonize…");
    expect(html).toContain(">acme<");
  });
});
