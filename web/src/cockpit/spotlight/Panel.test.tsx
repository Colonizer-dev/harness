// SpotlightPanel (issue #1228): every menu is built on it, and the keys are the same in each.
import type { ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../../api";
import { ApiContext } from "../../context";
import type { OrgEntry } from "../../orgs";
import type { Session } from "../../types";
import { ColonizePane } from "../Colonize";
import { InboxPanel } from "../NotificationsBell";
import { ModelSwitcher, scopeView, switchRequest, INSTALL_SCOPE } from "../ModelSwitcher";
import { roleDraft, draftLines, quickDraft, firstUsable } from "../ModelMenu";
import { WorkspacePanel } from "../WorkspaceMenu";
import { AskPanel, AskProvider, askButtonShown, askColony, askContextLabel, isAskKey } from "./Ask";
import { navAction, sectionIndex, startIndex, stepIndex } from "./panelNav";
import { SpotlightPanel, type PanelSection } from "./Panel";
import { SpotlightDialog, type SpotlightHost } from "./Spotlight";

const noop = () => {};
const anchor = { current: null };
const api = { chats: async () => ({ chats: [] }), loops: async () => [], modules: async () => [], issues: async () => [] } as unknown as Api;
const within = (node: ReactElement) => renderToStaticMarkup(<ApiContext.Provider value={api}>{node}</ApiContext.Provider>);

/** What makes a panel a Spotlight panel: the frosted surface, a search combobox, and the key-hint footer. */
function expectSpotlight(html: string) {
  expect(html).toContain("spot-panel");
  // The search field: a combobox, or a growing textarea where the text is also a task.
  expect(html).toMatch(/role="combobox"|<textarea/);
  expect(html).toContain("spot-footer");
  expect(html).toContain("spot-input");
}

const org = (name: string): OrgEntry => ({ org: name, live: 1, queued: 0, total: 3, pending: 0, avatar: null });
const colony = { id: "c43", repo: "acme/webshop", issue: 43, status: "running" } as unknown as Session;
const host: SpotlightHost = {
  sessions: [],
  repos: [],
  orgs: ["acme"],
  org: "acme",
  view: "home",
  colony: null,
  updateAvailable: false,
  onNavigate: noop,
  onOpenColony: noop,
  onOpenSettings: noop,
  onSelectOrg: noop,
  onOpenRepo: noop,
  onOpenChat: noop,
};

describe("every menu renders through SpotlightPanel", () => {
  it("the generic panel", () => {
    const sections: PanelSection[] = [{ id: "a", title: "Things", rows: [{ id: "1", title: "One", onPick: noop }] }];
    const html = renderToStaticMarkup(<SpotlightPanel label="things" placement="centered" query="" onQuery={noop} placeholder="Find…" sections={sections} onClose={noop} />);
    expectSpotlight(html);
    expect(html).toContain('aria-selected="true"');
  });
  it("the model menu, as a compact panel under the pill", () => {
    const html = renderToStaticMarkup(
      <ApiContext.Provider value={api}>
        <ModelSwitcher initialOpen initialAssignments={{ install: { module: "claude-code", roles: [{ role: "model", title: "Orchestrator model", value: "opus", source: "install", org_settable: true }] }, orgs: [], modules: [{ id: "claude-code", name: "Claude Code", roles: [], blocked: null }], models: [] }} initialRecent={[]} />
      </ApiContext.Provider>,
    );
    expectSpotlight(html);
    expect(html).toContain("spot-anchored");
    expect(html).toContain('placeholder="Switch model…"');
  });
  it("Colonize", () => {
    const html = within(
      <ColonizePane repos={[]} org={null} sessions={[]} githubConnected autopilotDefault={false} onCreated={noop} onOpenColony={noop} scope={[]} onLoadedCount={noop} onClose={noop} preloaded={{}} />,
    );
    expectSpotlight(html);
  });
  it("the workspace switcher", () => {
    const html = renderToStaticMarkup(<WorkspacePanel anchor={anchor} orgs={[org("acme"), org("initech")]} hiddenOrgs={[org("old")]} selectedOrg="acme" onSelect={noop} onOpenOrgSettings={noop} onManageOrgs={noop} onClose={noop} />);
    expectSpotlight(html);
    expect(html).toContain('placeholder="Switch workspace…"');
    expect(html).toContain("Switched off");
    expect(html).toContain("Manage orgs…");
    // It opens on where you are, not on the first row.
    expect(html).toMatch(/aria-selected="true"[^>]*>.*acme/);
  });
  it("the notifications", () => {
    const html = within(<InboxPanel anchor={anchor} onClose={noop} sessions={[]} readAt={0} onMarkAllRead={noop} onOpenColony={noop} onOpenInbox={noop} onOpenNotificationSettings={noop} />);
    expectSpotlight(html);
    expect(html).toContain('placeholder="Search notifications…"');
    expect(html).toContain("Open inbox");
  });
  it("Spotlight itself", () => {
    expectSpotlight(within(<SpotlightDialog host={host} onClose={noop} />));
  });
  it("the Ask panel", () => {
    expectSpotlight(
      within(
        <AskPanel anchor={anchor} host={host} colony={null} detached={false} onAttach={noop} onDetach={noop} turns={[]} chat={null} draft="" onDraft={noop} approvals={{ byId: {}, add: noop, decide: async () => null, busy: new Set() }} onSend={noop} onStop={noop} onReset={noop} onClose={noop} />,
      ),
    );
  });
});

describe("the same keys everywhere", () => {
  const key = (k: string, extra: { shiftKey?: boolean; isComposing?: boolean } = {}) => ({ key: k, shiftKey: false, ...extra });
  it("↑ ↓ move, ⇥ jumps sections, ↵ picks", () => {
    expect(navAction(key("ArrowDown"), 4, 2, true)).toEqual({ type: "move", delta: 1 });
    expect(navAction(key("ArrowUp"), 4, 2, true)).toEqual({ type: "move", delta: -1 });
    expect(navAction(key("Tab"), 4, 2, true)).toEqual({ type: "section", back: false });
    expect(navAction(key("Tab", { shiftKey: true }), 4, 2, true)).toEqual({ type: "section", back: true });
    expect(navAction(key("Enter"), 4, 2, true)).toEqual({ type: "pick" });
  });
  it("leaves ⇥ alone with one section, ↵ with nothing selected, ⇧↵ and composition", () => {
    expect(navAction(key("Tab"), 4, 1, true)).toBeNull();
    expect(navAction(key("Enter"), 4, 1, false)).toBeNull();
    expect(navAction(key("Enter", { shiftKey: true }), 4, 1, true)).toBeNull();
    expect(navAction(key("ArrowDown", { isComposing: true }), 4, 1, true)).toBeNull();
    expect(navAction(key("ArrowDown"), 0, 0, false)).toBeNull();
  });
  it("wraps, and starts on the primary row, else the current choice, else the first", () => {
    expect(stepIndex(3, 4, 1)).toBe(0);
    expect(stepIndex(0, 4, -1)).toBe(3);
    expect(stepIndex(-1, 4, 1)).toBe(0);
    expect(stepIndex(-1, 4, -1)).toBe(3);
    expect(sectionIndex(["a", "a", "b", "c"], 0, false)).toBe(2);
    expect(sectionIndex(["a", "a", "b", "c"], 3, false)).toBe(0);
    expect(sectionIndex(["a", "a", "b", "c"], 0, true)).toBe(3);
    expect(startIndex([{}, { checked: true }, { primary: true }], true)).toBe(2);
    expect(startIndex([{}, { checked: true }], true)).toBe(1);
    expect(startIndex([{}, {}], true)).toBe(0);
    expect(startIndex([{}, {}], false)).toBe(-1);
  });
});

describe("the model menu's role rows", () => {
  const view = scopeView(
    { install: { module: "claude-code", roles: [{ role: "model", title: "Orchestrator model", value: "opus", source: "install", org_settable: true }, { role: "subagent_model", title: "Subagent model", value: "", source: "default", org_settable: true }, { role: "summary_model", title: "Summary", value: "", source: "default", org_settable: false }] }, orgs: [], modules: [{ id: "claude-code", name: "Claude Code", roles: [{ role: "model", title: "m", org_settable: true }, { role: "subagent_model", title: "s", org_settable: true }, { role: "summary_model", title: "x", org_settable: false }], blocked: null }], models: [] },
    INSTALL_SCOPE,
  );
  it("switch the right role, and only that one", () => {
    const draft = roleDraft(view, INSTALL_SCOPE, { roles: {} }, "subagent_model", "zai/glm-5");
    expect(switchRequest(INSTALL_SCOPE, draft, "new")).toEqual({ scope: "install", roles: { subagent_model: "zai/glm-5" }, apply: "new" });
    // A second pick adds to the draft; a role the scope cannot set is left alone.
    const both = roleDraft(view, INSTALL_SCOPE, draft, "model", "sonnet");
    expect(both.roles).toEqual({ subagent_model: "zai/glm-5", model: "sonnet" });
    expect(roleDraft({ rows: view.rows.map((r) => ({ ...r, editable: false })) }, INSTALL_SCOPE, draft, "model", "x")).toBe(draft);
    expect(draftLines(view, both, [], INSTALL_SCOPE)).toEqual(["Orchestrator model → sonnet", "Subagent model → glm-5"]);
  });
  it("a quick switch moves every role the scope can set, but not the account fallback, to one model", () => {
    expect(quickDraft(view, INSTALL_SCOPE, { roles: {} }, "zai/glm-5").roles).toEqual({ model: "zai/glm-5", subagent_model: "zai/glm-5", summary_model: "zai/glm-5" });
    expect(firstUsable([{ id: "a/x", provider: "a", out_of_quota: true }, { id: "a/y", provider: "a", out_of_quota: false }] as never, "a")?.id).toBe("a/y");
  });
});

describe("the Ask panel", () => {
  const withProvider = (view: SpotlightHost["view"], colonyOnScreen: Session | null = null) =>
    within(
      <AskProvider host={{ ...host, view, colony: colonyOnScreen }}>
        <span />
      </AskProvider>,
    );
  it("has a button on every view except Chat", () => {
    for (const view of ["overview", "home", "colony", "launch", "inbox", "history", "loops", "settings", "memory", "host", "secrets", "code"] as const) {
      const html = withProvider(view);
      expect(html, view).toContain("data-ask-button");
      expect(html).toContain('title="Ask Colonizer · ');
      expect(html).toContain('aria-label="Ask Colonizer"');
    }
    expect(withProvider("chat")).not.toContain("data-ask-button");
    expect(askButtonShown("chat")).toBe(false);
  });
  it("toggles on ⌘J and Ctrl-J, but not inside the editor or a terminal", () => {
    const k = (over = {}) => ({ key: "j", metaKey: true, ctrlKey: false, altKey: false, shiftKey: false, defaultPrevented: false, target: null, ...over });
    expect(isAskKey(k())).toBe(true);
    expect(isAskKey(k({ metaKey: false, ctrlKey: true, key: "J" }))).toBe(true);
    expect(isAskKey(k({ key: "k" }))).toBe(false);
    expect(isAskKey(k({ metaKey: false }))).toBe(false);
    expect(isAskKey(k({ shiftKey: true }))).toBe(false);
    expect(isAskKey(k({ target: { closest: (s: string) => (s.includes(".xterm") ? {} : null) } }))).toBe(false);
  });
  const panel = (c: Session | null, extra: { turns?: never[]; detached?: boolean; onScreen?: Session | null } = {}) =>
    within(
      <AskPanel anchor={anchor} host={{ ...host, colony: extra.onScreen ?? null }} colony={c} detached={extra.detached ?? false} onAttach={noop} onDetach={noop} turns={[]} chat={null} draft="" onDraft={noop} approvals={{ byId: {}, add: noop, decide: async () => null, busy: new Set() }} onSend={noop} onStop={noop} onReset={noop} onClose={noop} />,
    );
  it("attaches the colony on screen as a chip, else the workspace", () => {
    expect(askColony(undefined, colony)).toBe(colony);
    expect(askColony(null, colony)).toBeNull();
    expect(askColony(colony, null)).toBe(colony);
    expect(askContextLabel(colony, "acme")).toBe("acme/webshop#43");
    expect(askContextLabel(null, "acme")).toBe("acme");
    expect(askContextLabel(null, null)).toBe("All workspaces");
    const attached = panel(colony);
    expect(attached).toContain('data-ask-context="colony"');
    expect(attached).toContain("acme/webshop#43");
    expect(attached).toContain('aria-label="detach this colony"');
    expect(attached).toContain("Summarize this colony");
    const org = panel(null);
    expect(org).toContain('data-ask-context="org"');
    expect(org).not.toContain("Summarize this colony");
    // After taking the chip off, the colony on screen can be put back.
    expect(panel(null, { detached: true, onScreen: colony })).toContain("+ acme/webshop#43");
  });
  it("offers the way into the full chat and the shortcut", () => {
    const html = panel(null);
    expect(html).toContain("Open full chat");
    expect(html).toContain("Ask Colonizer");
    expect(html).toContain("toggle");
  });
});
