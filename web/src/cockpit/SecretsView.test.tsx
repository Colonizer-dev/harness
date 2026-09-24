// Secrets (keychain store): the view never renders a value, and the mock keeps the server's rules —
// a save lands in the keychain unless the secret already lives on file, a move changes only where it
// lives, a removal falls back to the environment when one supplies the key.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import { providerSecretId } from "../secretsNav";
import { SecretsView, colonyAccessText } from "./SecretsView";

describe("SecretsView", () => {
  it("renders its frame before the list arrives, with no values anywhere", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={createMockApi()}>
        <SecretsView />
      </ApiContext.Provider>,
    );
    expect(html).toContain("Secrets");
    expect(html).toContain("Reading what this mothership holds");
    expect(html).not.toContain("type=\"text\"");
  });

  it("names a provider's key the way the Secrets page lists it", () => {
    expect(providerSecretId("zai")).toBe("provider-keys:zai");
  });
});

describe("mock secrets", () => {
  it("saves new keys to the keychain, keeps file keys on file, and moves both ways", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const api = createMockApi();
    const { keychain, secrets } = await api.secrets();
    expect(keychain.available).toBe(true);
    expect(JSON.stringify(secrets)).not.toMatch(/sk-|ghp_/);

    expect((await api.saveSecret("provider-keys:bailian", "sk-new")).location).toBe("keychain");
    expect((await api.saveSecret("github-token", "ghp_x")).location).toBe("file");
    expect((await api.moveSecret("github-token", "keychain")).location).toBe("keychain");
    expect((await api.moveSecret("github-token", "file")).location).toBe("file");
    const removed = await api.deleteSecret("voice-keys:openai");
    expect("location" in removed && removed.location).toBe("env");
    vi.useRealTimers();
  });

  it("adds a colony secret as an injected row, and removing it drops the row", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const api = createMockApi();
    await api.saveColonySecret({ env: "SENTRY_DSN", hosts: ["sentry.io"], scope: { kind: "org", org: "acme" }, value: "x" });
    const row = (await api.secrets()).secrets.find((r) => r.id === "colony:SENTRY_DSN");
    expect(row?.group).toBe("colonies");
    expect(row?.colonies).toEqual({ kind: "injected", hosts: ["sentry.io"] });
    expect(JSON.stringify(row)).not.toContain('"x"');
    expect(await api.deleteSecret("colony:SENTRY_DSN")).toEqual({ id: "colony:SENTRY_DSN", removed: true });
    expect((await api.secrets()).secrets.some((r) => r.id === "colony:SENTRY_DSN")).toBe(false);
    vi.useRealTimers();
  });
});

describe("colonyAccessText", () => {
  it("says plainly what a colony gets of each kind of secret", () => {
    expect(colonyAccessText({ kind: "gateway", hosts: [] }).text).toBe("Via gateway · never in the VM");
    expect(colonyAccessText({ kind: "injected", hosts: ["api.anthropic.com"] }).text).toBe("Injected for api.anthropic.com only");
    expect(colonyAccessText({ kind: "none", hosts: [] }).text).toBe("Not given to colonies");
  });
});
