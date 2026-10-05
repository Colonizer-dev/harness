// The "Added to <org>" notification (issue #176): a compact notification row with two inline
// actions, not a colony decision card. Rendered through react-dom/server like the rest of the
// cockpit's tests (no jsdom), so the click paths are tested through the helpers the buttons call.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { pendingOrgPrompts } from "../orgs";
import type { OrgInfo } from "../types";
import { LATER_HINT, OrgNotices, answerNewOrg, groupTitle } from "./OrgNotice";

const org = (name: string, overrides: Partial<OrgInfo> = {}): OrgInfo => ({
  org: name,
  colonies: { live: 0, total: 0 },
  pending_memory: 0,
  settings: {},
  awaiting_decision: true,
  ...overrides,
});

function render(orgs: OrgInfo[]): string {
  return renderToStaticMarkup(
    <ApiContext.Provider value={{} as Api}>
      <OrgNotices orgs={orgs} onAnswered={() => {}} />
    </ApiContext.Provider>,
  );
}

describe("OrgNotices", () => {
  it("renders one new org as a notification row with inline actions, not a decision card", () => {
    const out = render([org("initech")]);
    expect(out).toContain('aria-label="New organisation: initech"');
    expect(out).toContain("Added to <span");
    expect(out).toContain(">initech</span>");
    expect(out).toContain("Add it as a workspace so colonies can work on its repos.");
    expect(out).toContain(">Add workspace</button>");
    expect(out).toContain(">Not now</button>");
    expect(out).toContain('aria-label="Add initech as a workspace"');
    expect(out).toContain(`title="${LATER_HINT}"`);
    // None of the decision card's pieces: no radios, no Confirm, no decision count.
    expect(out).not.toContain('type="radio"');
    expect(out).not.toContain("Confirm");
    expect(out).not.toMatch(/decision/i);
  });

  it("falls back to the building chip when the org has no avatar", () => {
    expect(render([org("initech")])).toContain("bg-accent-soft text-accent");
    expect(render([org("initech", { avatar_url: "https://example.com/a.png" })])).toContain('src="https://example.com/a.png"');
  });

  it("groups more than one new org into a single expandable row", () => {
    const out = render([org("alpha"), org("beta"), org("gamma")]);
    expect(groupTitle(3)).toBe("Added to 3 organisations");
    expect(out).toContain('aria-label="Added to 3 organisations"');
    expect(out).toContain('aria-expanded="false"');
    expect(out).toContain("alpha · beta · gamma");
    // Collapsed: the per-org actions stay behind the expand.
    expect(out).not.toContain("Add workspace");
    expect(out).not.toMatch(/decision/i);
  });

  it("renders nothing once every org is answered", () => {
    expect(render([])).toBe("");
  });
});

describe("answering", () => {
  const saved = (enabled: boolean) => ({ org: "initech", settings: { enabled } });

  it("Add workspace saves {enabled: true} and the row goes", async () => {
    const saveOrg = vi.fn(async (_org: string, settings: { enabled?: boolean | null }) => saved(settings.enabled === true));
    const reply = await answerNewOrg({ saveOrg } as unknown as Pick<Api, "saveOrg">, "initech", true);
    expect(saveOrg).toHaveBeenCalledWith("initech", { enabled: true });
    expect(reply.settings.enabled).toBe(true);
    expect(pendingOrgPrompts([org("initech")], new Set(["initech"]))).toEqual([]);
  });

  it("Not now saves {enabled: false} and the row goes", async () => {
    const saveOrg = vi.fn(async (_org: string, settings: { enabled?: boolean | null }) => saved(settings.enabled === true));
    await answerNewOrg({ saveOrg } as unknown as Pick<Api, "saveOrg">, "initech", false);
    expect(saveOrg).toHaveBeenCalledWith("initech", { enabled: false });
    expect(pendingOrgPrompts([org("initech"), org("hooli")], new Set(["initech"])).map((o) => o.org)).toEqual(["hooli"]);
  });
});
