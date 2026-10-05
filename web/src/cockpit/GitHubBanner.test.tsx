// The GitHub pause banner (issue #1074): one top-level line with the cause and the next step while
// GitHub refuses the account, the right action for each cause, and nothing when GitHub works.
// Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { GitHubPause } from "../features/providers/types";
import { GitHubBanner, gitHubPauseText } from "./GitHubBanner";

const now = new Date("2026-10-01T09:00:00Z");

const pause = (overrides: Partial<GitHubPause> = {}): GitHubPause => ({
  paused: true,
  cause: "suspended",
  message: "GitHub account suspended",
  next_step: "contact GitHub support",
  since: "2026-10-01T08:30:00Z",
  next_probe_at: "2026-10-01T09:25:00Z",
  probes: 1,
  detail: "gh: Sorry. Your account was suspended. (HTTP 403)",
  queued: 36,
  held_publishes: 1,
  refused_calls: 12,
  ...overrides,
});

const markup = (p: GitHubPause | null | undefined) =>
  renderToStaticMarkup(<GitHubBanner pause={p} onReconnect={() => {}} now={now} />);

describe("gitHubPauseText", () => {
  it("names the cause, the next step, what waits and the next check", () => {
    expect(gitHubPauseText(pause(), now)).toBe(
      "GitHub account suspended: contact GitHub support. Launches, publishes, merges and GitHub writes are paused " +
        "(36 queued colonies wait, 1 publish is held); running colonies keep working. Checking GitHub again in 25 minutes.",
    );
  });

  it("says what to do about a revoked token and leaves out what is not held", () => {
    expect(
      gitHubPauseText(
        pause({
          cause: "token_revoked",
          message: "Token revoked",
          next_step: "reconnect GitHub in Settings → Connections",
          queued: 1,
          held_publishes: 0,
          next_probe_at: "2026-10-01T09:00:30Z",
        }),
        now,
      ),
    ).toBe(
      "Token revoked: reconnect GitHub in Settings → Connections. Launches, publishes, merges and GitHub writes are " +
        "paused (1 queued colony waits); running colonies keep working. Checking GitHub again within a minute.",
    );
  });
});

describe("GitHubBanner", () => {
  it("renders one alert with GitHub support for a suspension", () => {
    const out = markup(pause());
    expect(out.match(/role="alert"/g)).toHaveLength(1);
    expect(out).toContain("GitHub account suspended: contact GitHub support.");
    expect(out).toContain('href="https://support.github.com"');
    expect(out).not.toContain("Reconnect GitHub");
  });

  it("offers Reconnect GitHub for a revoked token", () => {
    const out = markup(pause({ cause: "token_revoked", message: "Token revoked", next_step: "reconnect GitHub in Settings → Connections" }));
    expect(out).toContain("Token revoked: reconnect GitHub in Settings → Connections.");
    expect(out).toContain(">Reconnect GitHub<");
  });

  it("offers no button while it waits out a secondary rate limit", () => {
    const out = markup(pause({ cause: "secondary_rate_limit", message: "GitHub secondary rate limit", next_step: "nothing to do" }));
    expect(out).toContain("GitHub secondary rate limit: nothing to do.");
    expect(out).not.toContain("<button");
    expect(out).not.toContain("<a ");
  });

  it("renders nothing while GitHub works or on an older mothership", () => {
    expect(markup({ paused: false })).toBe("");
    expect(markup(null)).toBe("");
    expect(markup(undefined)).toBe("");
  });
});
