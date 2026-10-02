// The launch url's pure half (issue #745): the view a manifest shortcut asks for, the GitHub issue a
// shared link names, the colony that holds it, and the params Cockpit strips once it has applied
// them. The iOS guess is pinned here too, so the Home-Screen sheet can only ever show on iOS.
import { describe, expect, it } from "vitest";

import { holdingSession, isIosSafari, sharedIssueFromUrl, sessionWithPull, stripLaunchParams, viewFromUrl, welcomeFromUrl } from "./launchUrl";
import type { Session } from "./types";

const iPhoneUA = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const iPadUA = "Mozilla/5.0 (iPad; CPU OS 16_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.6 Mobile/15E148 Safari/604.1";
const iPadAsMacUA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.6 Safari/605.1.15";
const macSafariUA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";
const iosChromeUA =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/124.0.6367.111 Mobile/15E148 Safari/604.1";
const androidChromeUA =
  "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Mobile Safari/537.36";
const windowsChromeUA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36";

describe("viewFromUrl", () => {
  it("names the view a manifest shortcut's url asks for", () => {
    expect(viewFromUrl("/?view=inbox")).toBe("inbox");
    expect(viewFromUrl("https://cockpit.test/?view=launch&mock=1")).toBe("launch");
    expect(viewFromUrl("/?mock=1&view=home")).toBe("home");
    expect(viewFromUrl("/?view=overview")).toBe("overview");
  });

  it("keeps an unknown or stale view from hijacking the cockpit", () => {
    expect(viewFromUrl("/?view=colony")).toBe("colony"); // a real view, reached only by opening a colony
    expect(viewFromUrl("/?view=settings")).toBe("settings");
    expect(viewFromUrl("/?view=does-not-exist")).toBeNull();
    expect(viewFromUrl("/?view=")).toBeNull();
    expect(viewFromUrl("/?mock=1")).toBeNull();
    expect(viewFromUrl("/")).toBeNull();
    expect(viewFromUrl("not a url")).toBeNull();
  });
});

describe("sharedIssueFromUrl", () => {
  const issue = (params: string) => sharedIssueFromUrl(`/?mock=1${params}`);

  it("reads the share target's own param first", () => {
    expect(sharedIssueFromUrl("/?share_url=https%3A%2F%2Fgithub.com%2Facme%2Fwebshop%2Fissues%2F42")).toEqual({
      repo: "acme/webshop",
      number: 42,
      kind: "issue",
    });
  });

  it("finds the link wherever Android's share sheet put it — text most often", () => {
    const text = encodeURIComponent('Look at this bug https://github.com/acme/webshop/issues/42 — checkout is broken');
    expect(issue(`&share_title=${encodeURIComponent("Checkout is broken")}&share_text=${text}`)).toEqual({
      repo: "acme/webshop",
      number: 42,
      kind: "issue",
    });
  });

  it("reads pull requests as pulls, trailing fragments and all", () => {
    const pr = encodeURIComponent("https://github.com/acme/webshop/pull/612#issuecomment-2543210");
    expect(sharedIssueFromUrl(`/?share_url=${pr}`)).toEqual({ repo: "acme/webshop", number: 612, kind: "pull" });
    const files = encodeURIComponent("https://github.com/acme/webshop/pull/7/files");
    expect(sharedIssueFromUrl(`/?share_url=${files}`)).toEqual({ repo: "acme/webshop", number: 7, kind: "pull" });
  });

  it("prefers share_url over the chattier params when several carry a link", () => {
    const url = encodeURIComponent("https://github.com/acme/webshop/issues/42");
    const text = encodeURIComponent("https://github.com/acme/design-system/issues/18");
    expect(sharedIssueFromUrl(`/?share_url=${url}&share_text=${text}`)).toMatchObject({ repo: "acme/webshop" });
  });

  it("takes the first link when one shared text carries two", () => {
    const text = encodeURIComponent("fix https://github.com/acme/webshop/issues/42 after https://github.com/acme/webshop/issues/43 lands");
    expect(sharedIssueFromUrl(`/?share_text=${text}`)).toMatchObject({ number: 42 });
  });

  it("never mistakes a long number, a list or another host for one issue", () => {
    expect(sharedIssueFromUrl(`/?share_url=${encodeURIComponent("https://github.com/acme/webshop/pull/612")}`)).toMatchObject({ number: 612 });
    expect(sharedIssueFromUrl(`/?share_url=${encodeURIComponent("https://github.com/acme/webshop/issues")}`)).toBeNull();
    expect(sharedIssueFromUrl(`/?share_url=${encodeURIComponent("https://github.com/acme/webshop/issues?q=is%3Aopen")}`)).toBeNull();
    expect(sharedIssueFromUrl(`/?share_url=${encodeURIComponent("https://github.com/acme/webshop")}`)).toBeNull();
    expect(sharedIssueFromUrl(`/?share_url=${encodeURIComponent("https://gitlab.com/acme/webshop/issues/42")}`)).toBeNull();
    expect(sharedIssueFromUrl("/?share_text=no link here")).toBeNull();
    expect(sharedIssueFromUrl("/?mock=1")).toBeNull();
  });
});

describe("holdingSession", () => {
  const session = (over: Partial<Session> & Pick<Session, "repo" | "issue">): Session => ({
    id: "demo1234",
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: "colonizer/issue-42",
    base: "main",
    worktree: "wt",
    git_admin_dir: null,
    sandbox: "sb",
    mesh: null,
    agent: "claude",
    autopilot: false,
    pr_url: null,
    error: null,
    cleaned_up: false,
    keep_worktree: false,
    cost_usd: null,
    created_at: "2026-09-01T10:00:00Z",
    updated_at: "2026-09-01T10:00:00Z",
    ...over,
  });
  const sessions = [
    session({ id: "demo1234", repo: "acme/webshop", issue: 42 }),
    session({ id: "old9876", repo: "acme/webshop", issue: 37, status: "pr_opened", pr_url: "https://github.com/acme/webshop/pull/61" }),
    session({ id: "merge_w1", repo: "acme/webshop", issue: 58, status: "merged", pr_url: "https://github.com/acme/webshop/pull/71" }),
  ];

  it("names the colony that holds the shared issue", () => {
    expect(holdingSession(sessions, { repo: "acme/webshop", number: 42, kind: "issue" })?.id).toBe("demo1234");
  });

  it("leaves a free issue to the launch form", () => {
    expect(holdingSession(sessions, { repo: "acme/webshop", number: 43, kind: "issue" })).toBeNull();
    expect(holdingSession(sessions, { repo: "acme/design-system", number: 42, kind: "issue" })).toBeNull();
  });

  it("a merged colony no longer holds its issue", () => {
    expect(holdingSession(sessions, { repo: "acme/webshop", number: 58, kind: "issue" })).toBeNull();
  });

  it("a pull request matches the session that records its PR url, and only that", () => {
    expect(holdingSession(sessions, { repo: "acme/webshop", number: 61, kind: "pull" })?.id).toBe("old9876");
    // A PR number must never collide with the unrelated issue carrying the same number.
    expect(holdingSession(sessions, { repo: "acme/webshop", number: 43, kind: "pull" })).toBeNull();
  });

  it("reads a PR number past its prefix, so /pull/612 is not /pull/61", () => {
    expect(sessionWithPull([{ ...sessions[0], pr_url: "https://github.com/acme/webshop/pull/612" }], "acme/webshop", 61)).toBeNull();
    expect(sessionWithPull([{ ...sessions[0], pr_url: "https://github.com/acme/webshop/pull/61" }], "acme/webshop", 61)?.id).toBe("demo1234");
    expect(sessionWithPull([{ ...sessions[0], pr_url: "https://github.com/acme/webshop-fork/pull/61" }], "acme/webshop", 61)).toBeNull();
  });
});

describe("stripLaunchParams", () => {
  it("drops the launch params and keeps everything else", () => {
    expect(stripLaunchParams("/?mock=1&view=inbox")).toBe("/?mock=1");
    expect(stripLaunchParams("/?share_url=https%3A%2F%2Fgithub.com%2Fa%2Fb%2Fissues%2F1&mock=1")).toBe("/?mock=1");
    expect(stripLaunchParams("/?view=inbox&colony=demo1234&mock=1")).toBe("/?colony=demo1234&mock=1");
    expect(stripLaunchParams("/?share_title=Hi&share_text=Read&share_url=https%3A%2F%2Fgithub.com%2Fa%2Fb%2Fpull%2F1")).toBe("/");
    // A paired phone's welcome (issue #746) must not survive into a reloadable address bar.
    expect(stripLaunchParams("/?welcome=phone&mock=1")).toBe("/?mock=1");
    expect(stripLaunchParams("/")).toBe("/");
    expect(stripLaunchParams("http://[bad")).toBe("http://[bad");
  });
});

describe("welcomeFromUrl", () => {
  it("reads the phone sign-in landing, and nothing else", () => {
    expect(welcomeFromUrl("/?welcome=phone")).toBe("phone");
    expect(welcomeFromUrl("https://cockpit.test/?mock=1&welcome=phone")).toBe("phone");
    expect(welcomeFromUrl("/?welcome=")).toBeNull();
    expect(welcomeFromUrl("/?welcome=someday")).toBeNull();
    expect(welcomeFromUrl("/")).toBeNull();
    expect(welcomeFromUrl("not a url")).toBeNull();
  });
});

describe("isIosSafari", () => {
  it("says yes to iPhone, iPod and iPad", () => {
    expect(isIosSafari(iPhoneUA)).toBe(true);
    expect(isIosSafari(iPadUA)).toBe(true);
    expect(isIosSafari("Mozilla/5.0 (iPod touch; CPU iPhone OS 15_8 like Mac OS X) AppleWebKit/605.1.15")).toBe(true);
  });

  it("says yes to iPadOS posing as a Mac, by its touch points", () => {
    expect(isIosSafari(iPadAsMacUA, 5)).toBe(true);
    expect(isIosSafari(iPadAsMacUA, 0)).toBe(false);
    expect(isIosSafari(iPadAsMacUA, 1)).toBe(false);
  });

  it("says no to desktop Safari, Chrome, Firefox and Android", () => {
    expect(isIosSafari(macSafariUA, 0)).toBe(false);
    expect(isIosSafari(windowsChromeUA, 0)).toBe(false);
    expect(isIosSafari(androidChromeUA, 5)).toBe(false);
    expect(isIosSafari("", 0)).toBe(false);
  });

  it("says yes to other browsers on iOS — since 16.4 they all offer Add to Home Screen too", () => {
    expect(isIosSafari(iosChromeUA, 5)).toBe(true);
  });
});
