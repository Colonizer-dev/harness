// The cockpit-global Claude account banner (issue #984): it banners an account that needs the
// owner (a rejected sign-in or an exhausted plan), one line per alert, pluralises the waiting
// count, drops the waiting sentence at zero, and stays hidden without any alert. Rendered to
// static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { AccountAlert } from "../features/providers/types";
import { AccountBanner, accountAlertText } from "./AccountBanner";

const alert = (overrides: Partial<AccountAlert> = {}): AccountAlert => ({
  account: "default",
  state: "needs_sign_in",
  class: "auth",
  status: 401,
  since: "2026-09-18T09:00:00Z",
  waiting: 0,
  ...overrides,
});

const markup = (alerts: AccountAlert[] | null | undefined) =>
  renderToStaticMarkup(<AccountBanner alerts={alerts} onSignIn={() => {}} />);

describe("accountAlertText", () => {
  it("names the account and its state", () => {
    expect(accountAlertText(alert({ waiting: 10 }))).toBe(
      "Claude account default needs you to sign in again. 10 colonies are waiting on it.",
    );
    expect(accountAlertText(alert({ state: "limited", waiting: 3 }))).toBe(
      "Claude account default hit its usage limit. 3 colonies are waiting on it.",
    );
  });

  it("pluralises one colony and omits the waiting sentence at zero", () => {
    expect(accountAlertText(alert({ waiting: 1 }))).toBe(
      "Claude account default needs you to sign in again. 1 colony is waiting on it.",
    );
    expect(accountAlertText(alert({ waiting: 0 }))).toBe(
      "Claude account default needs you to sign in again.",
    );
  });
});

describe("AccountBanner", () => {
  it("renders one line per alert, each with a Sign in link", () => {
    const out = markup([alert({ waiting: 2 }), alert({ account: "work", state: "limited", waiting: 1 })]);
    expect(out.match(/role="status"/g)).toHaveLength(2);
    expect(out).toContain("Claude account default needs you to sign in again. 2 colonies are waiting on it.");
    expect(out).toContain("Claude account work hit its usage limit. 1 colony is waiting on it.");
    expect(out.match(/>Sign in</g)).toHaveLength(2);
  });

  it("renders nothing without any alert", () => {
    expect(markup([])).toBe("");
    expect(markup(null)).toBe("");
    expect(markup(undefined)).toBe("");
  });
});
