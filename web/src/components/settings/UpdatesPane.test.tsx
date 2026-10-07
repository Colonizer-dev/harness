// Release notes render as text, not raw Markdown, and an update that cannot be applied says why
// (issue #1125). Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { Api } from "../../api";
import { ApiContext } from "../../context";
import type { UpdateStatus } from "../../types";
import { UpdatesPane } from "./UpdatesPane";

const base: UpdateStatus = {
  enabled: true,
  blocked_by: null,
  installed: { version: "v0.2.6", commit: null, dirty: false, built_at: "2026-10-01T00:00:00Z", release: "v0.2.6" },
  latest: { version: "v0.2.7", url: "https://example.invalid", notes: "", published_at: null },
  available: true,
  last_checked: null,
  error: null,
  can_apply: { ok: true, reason: null },
  apply: { phase: "idle", version: null, started_at: null, error: null, log: "", colonies: [], backup: null },
  notices: [],
  behind: [],
  restarts: { restarting: [], failed: {} },
  switch_to_releases: null,
};

const render = (update: UpdateStatus) =>
  renderToStaticMarkup(
    <ApiContext.Provider value={{} as Api}>
      <UpdatesPane update={update} onChanged={() => {}} />
    </ApiContext.Provider>,
  );

const text = (html: string) => html.replace(/<[^>]*>/g, "");

describe("UpdatesPane release notes", () => {
  const notes = [
    "<!-- Prepended by .github/workflows/release.yml -->",
    "### What's new",
    "",
    "- **Faster** starts, see [the guide](https://example.com/guide?a=1&b=2)",
    "- bad [link](javascript:alert(1)) and <script>alert(1)</script>",
  ].join("\n");

  it("shows no raw Markdown, no comment, and no raw HTML", () => {
    const html = render({ ...base, latest: { ...base.latest!, notes } });
    const shown = text(html);
    expect(shown).not.toContain("&lt;!--");
    expect(html).not.toContain("<!--");
    expect(shown).not.toContain("###");
    expect(shown).not.toContain("**");
    expect(shown).toContain("What&#x27;s new");
    expect(html).not.toContain("<script>");
    expect(html).not.toContain("javascript:");
    expect(html).toContain('href="https://example.com/guide?a=1&amp;b=2"');
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).toContain("<strong>Faster</strong>");
  });
});

describe("UpdatesPane when it cannot apply", () => {
  it("shows the reason and the way to releases, with no spinner", () => {
    const html = render({
      ...base,
      can_apply: { ok: false, reason: "no installer found next to this build" },
      switch_to_releases: { reason: "no installer", command: "curl -fsSL https://colonizer.dev/install.sh | sh", then: "Then restart." },
    });
    expect(html).toContain("no installer found next to this build");
    expect(html).toContain("curl -fsSL https://colonizer.dev/install.sh");
    expect(html).not.toContain("animate-spin");
    expect(html).toContain("disabled");
  });

  it("says plainly that a development build is one", () => {
    const html = render({
      ...base,
      can_apply: { ok: false, reason: "development build" },
      switch_to_releases: { reason: "v0.2.6-dev is a development build, which holds work no release contains", command: "colonizer update --force", then: "x" },
    });
    expect(html).toContain("This is a development build");
    expect(html).toContain("colonizer update --force");
  });
});
