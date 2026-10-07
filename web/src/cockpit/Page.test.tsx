// The shared page container: every routed cockpit view sits in <Page>, so each gets the same
// fluid frame and gutters (and the next view added gets them by default). The Nest is the one
// exception: it is a full-bleed canvas and keeps its own layout.
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiContext } from "../context";
import { createMockApi } from "../mock";
import { Cockpit, COCKPIT_VIEWS } from "./Cockpit";
import type { CockpitView } from "./NavRail";
import { Page } from "./Page";
import { session } from "./testFixtures";

/** The test environment has no DOM: just enough window for the cockpit's stored view and media queries. */
function stubWindow(view: CockpitView) {
  const store = new Map<string, string>([["colonizer.cockpitView", view]]);
  vi.stubGlobal("window", {
    localStorage: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    },
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    location: { search: "", pathname: "/", hash: "" },
    addEventListener() {},
    removeEventListener() {},
  });
}

function renderCockpit(view: CockpitView): string {
  stubWindow(view);
  const noop = () => {};
  return renderToStaticMarkup(
    <ApiContext.Provider value={createMockApi()}>
      <Cockpit
        sessions={[session()]}
        sessionsLoaded
        orgs={[]}
        selectedOrg="acme"
        onSelectOrg={noop}
        selectedId={null}
        onSelectSession={noop}
        onOpenColony={noop}
        status={null}
        update={null}
        autopilotDefault={false}
        launchRequests={0}
        settingsRequests={0}
        settings={() => <div data-testid="settings-slot" />}
        onSessionChanged={noop}
        onCreated={noop}
        onOpenSettings={noop}
        colony={<div data-testid="colony-slot" />}
        memory={<div data-testid="memory-slot" />}
      />
    </ApiContext.Provider>,
  );
}

describe("Page", () => {
  it("gives a wide view the scroll root and the shared frame", () => {
    const html = renderToStaticMarkup(<Page frameClassName="flex flex-col gap-10">x</Page>);
    expect(html).toContain('data-page="wide"');
    expect(html).toContain('class="page-frame flex flex-col gap-10"');
    expect(html).not.toContain("max-w-");
  });

  it("marks readable and full-bleed pages, and a full page's column cap", () => {
    expect(renderToStaticMarkup(<Page width="readable">x</Page>)).toContain('data-page="readable"');
    const full = renderToStaticMarkup(
      <Page width="full" cap="readable">
        x
      </Page>,
    );
    expect(full).toContain('data-page="full"');
    expect(full).toContain('data-cap="readable"');
    expect(full).not.toContain("page-frame");
  });
});

describe("every routed view uses the shared page container", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  const routed = COCKPIT_VIEWS.filter((v) => v !== "home");

  it.each(routed)("%s", (view) => {
    const html = renderCockpit(view);
    expect(html).toMatch(/data-page="(wide|readable|full)"/);
    // No view sets its own narrow cap on its root any more.
    expect(html).not.toContain("max-w-[1080px]");
  });

  it("leaves the Nest as it is: no page container", () => {
    expect(renderCockpit("home")).not.toContain("data-page=");
  });
});
