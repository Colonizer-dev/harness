// The settings menu's three shapes, rendered to static markup (issue #1180): the sidebar, the icon
// rail and the phone list read one model, mark the page you are on, and draw a dot only when
// something needs the person.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { GROUPS, worstAttention, type Attention } from "./nav";
import { NeedsYou, PageChips, SettingsList, SettingsRail, SettingsSidebar, type NavGroupModel } from "./SettingsNav";

const search = { box: <input aria-label="Search settings" />, results: null };

function model(attention: Partial<Record<string, Attention>> = {}): NavGroupModel[] {
  const pages = (group: string) =>
    group === "models"
      ? [
          { id: "providers" as const, label: "Model providers", hint: "Claude and others", attention: attention.providers ?? null },
          { id: "module:agent" as const, label: "Agent", hint: "The coding agent" },
        ]
      : group === "general"
        ? [{ id: "cockpit" as const, label: "Your cockpit", hint: "The address" }]
        : [];
  return GROUPS.map((info) => {
    const list = pages(info.id);
    return { info, pages: list, attention: worstAttention(list.map((p) => ("attention" in p ? p.attention : null))) };
  });
}

describe("the sidebar", () => {
  it("opens only the group you are in, marks the page with aria-current, and shows each group's one-liner", () => {
    const html = renderToStaticMarkup(<SettingsSidebar groups={model()} active="providers" onSelect={() => {}} search={search} />);
    expect(html).toContain('aria-current="page"');
    expect(html.match(/aria-current="page"/g)).toHaveLength(1);
    expect(html).toContain("Model providers");
    expect(html).not.toContain('title="The address"'); // General is closed
    for (const group of GROUPS) expect(html).toContain(group.blurb);
    expect(html).toContain('aria-label="Search settings"');
  });

  it("draws a dot only where something needs you or is broken", () => {
    const calm = renderToStaticMarkup(<SettingsSidebar groups={model()} active="providers" onSelect={() => {}} search={search} />);
    expect(calm).not.toContain("needs you");
    expect(calm).not.toContain("broken");
    const amber = renderToStaticMarkup(<SettingsSidebar groups={model({ providers: "warn" })} active="providers" onSelect={() => {}} search={search} />);
    expect(amber).toContain("bg-warn");
    expect(amber).toContain("needs you");
    const red = renderToStaticMarkup(<SettingsSidebar groups={model({ providers: "err" })} active="providers" onSelect={() => {}} search={search} />);
    expect(red).toContain("bg-err");
    expect(red).toContain("broken");
  });

  it("swaps the groups for the search results while there are some", () => {
    const html = renderToStaticMarkup(
      <SettingsSidebar groups={model()} active="providers" onSelect={() => {}} search={{ box: search.box, results: <p>3 results</p> }} />,
    );
    expect(html).toContain("3 results");
    expect(html).not.toContain("Which AI models colonies may use");
  });
});

describe("the icon rail", () => {
  it("gives every group an accessible name and lights the open one", () => {
    const html = renderToStaticMarkup(<SettingsRail groups={model()} active="module:agent" onSelect={() => {}} />);
    for (const group of GROUPS) expect(html).toContain(`aria-label="${group.label.replace("&", "&amp;")}"`);
    expect(html.match(/aria-current="true"/g)).toHaveLength(1);
  });

  it("lists the open group's pages as chips, and none for a group with one page", () => {
    const groups = model();
    const chips = renderToStaticMarkup(<PageChips group={groups.find((g) => g.info.id === "models")} active="providers" onSelect={() => {}} />);
    expect(chips).toContain("Agent");
    expect(chips).toContain('aria-current="page"');
    expect(renderToStaticMarkup(<PageChips group={groups.find((g) => g.info.id === "general")} active="cockpit" onSelect={() => {}} />)).toBe("");
  });
});

describe("the phone list", () => {
  it("lists every page of every group with its hint, one tap from its page", () => {
    const html = renderToStaticMarkup(<SettingsList groups={model()} onSelect={() => {}} search={search} />);
    expect(html).toContain("Your cockpit");
    expect(html).toContain("Claude and others");
    expect(html).toContain('data-page-id="providers"');
  });
});

describe("NeedsYou", () => {
  it("renders nothing with nothing to say", () => {
    expect(renderToStaticMarkup(<NeedsYou items={[]} />)).toBe("");
  });

  it("says needs you for amber, broken for red, and offers the fix as a button", () => {
    const amber = renderToStaticMarkup(<NeedsYou items={[{ tone: "warn", text: "One provider needs a key", action: { label: "Set key", run: () => {} } }]} />);
    expect(amber).toContain("Needs you");
    expect(amber).toContain("Set key");
    const red = renderToStaticMarkup(<NeedsYou items={[{ tone: "warn", text: "a" }, { tone: "err", text: "b" }]} />);
    expect(red).toContain("Something is broken");
  });
});
