// Settings → API tokens (issue #646), rendered to static markup like the cockpit's other tests.
// Under renderToStaticMarkup the pane's fetch never answers, so the row and the one-time secret
// reveal are rendered directly; the form → POST payload mapping is pinned on the helper.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { ApiTokenMeta, CreatedApiToken } from "../types";
import { SecretReveal, TOKEN_SCOPES, TokenRow, buildNewToken, capProblem, capsText, limitsText, parseList, toMeta, usedText } from "./TokensPane";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(node);

const launchToken: ApiTokenMeta = {
  id: "tok_nightly7",
  name: "nightly burn-down",
  scope: "launch",
  orgs: [],
  repos: [],
  max_concurrent: 2,
  budget_usd_per_day: 5,
  created_at: "2026-09-16T00:00:00Z",
  last_used_at: "2026-09-28T10:00:00Z",
};
const plainToken: ApiTokenMeta = {
  ...launchToken,
  id: "tok_wallboard",
  name: "wallboard",
  scope: "read",
  max_concurrent: undefined,
  budget_usd_per_day: undefined,
  last_used_at: undefined,
};
const limitedToken: ApiTokenMeta = {
  ...launchToken,
  id: "tok_phoneops",
  name: "phone ops",
  scope: "operate",
  orgs: ["acme"],
  repos: ["acme/webshop"],
  max_concurrent: undefined,
  budget_usd_per_day: undefined,
};

const baseForm = { name: "  ci  ", scope: "operate" as const, orgs: " acme, globex ", repos: "acme/webshop acme/other", maxConcurrent: " 2 ", budgetPerDay: "" };

describe("TokensPane helpers", () => {
  it("splits a list field on commas and whitespace, dropping the empties", () => {
    expect(parseList("acme, globex\nother")).toEqual(["acme", "globex", "other"]);
    expect(parseList(" , ")).toEqual([]);
    expect(parseList("")).toEqual([]);
  });

  it("builds the POST payload: name trimmed, lists split, blank caps absent", () => {
    const payload = buildNewToken(baseForm);
    expect(payload.name).toBe("ci");
    expect(payload.scope).toBe("operate");
    expect(payload.orgs).toEqual(["acme", "globex"]);
    expect(payload.repos).toEqual(["acme/webshop", "acme/other"]);
    expect(payload.max_concurrent).toBe(2);
    expect("budget_usd_per_day" in payload).toBe(false);
    // An empty limit list means no limit on the server too, but the field is left off entirely.
    expect("orgs" in buildNewToken({ ...baseForm, orgs: " " })).toBe(false);
    expect("repos" in buildNewToken({ ...baseForm, repos: "" })).toBe(false);
  });

  it("includes a well-formed cap and leaves everything else about it off", () => {
    expect(buildNewToken({ ...baseForm, maxConcurrent: "2", budgetPerDay: "1.5" })).toMatchObject({ max_concurrent: 2, budget_usd_per_day: 1.5 });
    expect("max_concurrent" in buildNewToken({ ...baseForm, maxConcurrent: "soon" })).toBe(false);
  });

  it("refuses bad caps before anything is sent, naming the field", () => {
    expect(capProblem(baseForm)).toBeNull();
    for (const bad of ["0", "-1", "2.5", "soon"]) expect(capProblem({ ...baseForm, maxConcurrent: bad })).toMatch(/max_concurrent/);
    for (const bad of ["0", "-1", "soon"]) expect(capProblem({ ...baseForm, budgetPerDay: bad })).toMatch(/budget_usd_per_day/);
    expect(capProblem({ ...baseForm, budgetPerDay: "1.5" })).toBeNull();
  });

  it("drops the plaintext when a create answer joins the list", () => {
    const created: CreatedApiToken = { ...launchToken, token: "col_" + "cd".repeat(32) };
    const meta = toMeta(created);
    expect("token" in meta).toBe(false);
    expect(meta).toEqual({ ...launchToken });
  });

  it("reads the limits as one line, `all repositories` when there are none", () => {
    expect(limitsText(launchToken)).toBe("all repositories");
    expect(limitsText(limitedToken)).toBe("orgs acme · repos acme/webshop");
    expect(limitsText({ orgs: ["acme"], repos: [] })).toBe("orgs acme");
  });

  it("reads the caps as one line, `no caps` when there is neither", () => {
    expect(capsText(launchToken)).toBe("max 2 at a time · $5/day");
    expect(capsText(plainToken)).toBe("no caps");
    expect(capsText({ ...launchToken, max_concurrent: undefined })).toBe("$5/day");
  });

  it("says a token that never made a request was never used", () => {
    const now = new Date("2026-09-28T13:00:00Z");
    expect(usedText(plainToken, now)).toBe("never used");
    expect(usedText(launchToken, now)).toBe("3h ago");
  });

  it("offers the scopes lowest privilege first, each with a hint", () => {
    expect(TOKEN_SCOPES.map((scope) => scope.id)).toEqual(["read", "operate", "launch"]);
    for (const scope of TOKEN_SCOPES) expect(scope.hint.length).toBeGreaterThan(0);
  });
});

describe("TokenRow", () => {
  it("shows the name, the scope badge, limits, caps and both dates", () => {
    const html = wrap(<TokenRow token={launchToken} actions={<button>Revoke</button>} />);
    expect(html).toContain("nightly burn-down");
    expect(html).toContain(">launch</span>");
    expect(html).toContain("all repositories");
    expect(html).toContain("max 2 at a time · $5/day");
    expect(html).toContain("created ");
    expect(html).toContain("last used ");
    expect(html).toContain(">Revoke</button>");
  });

  it("marks a never-used token as such instead of a blank last-used date", () => {
    const html = wrap(<TokenRow token={plainToken} />);
    expect(html).toContain(">read</span>");
    expect(html).toContain("no caps");
    expect(html).toContain("never used");
    expect(html).not.toContain("last used");
  });
});

describe("SecretReveal", () => {
  const created: CreatedApiToken = { ...launchToken, token: "col_" + "ab".repeat(32) };

  it("shows the plaintext once, with the copy-now warning and a Done that clears it", () => {
    const html = wrap(<SecretReveal created={created} onCopy={() => {}} onDone={() => {}} />);
    expect(html).toContain("col_" + "ab".repeat(32));
    expect(html).toContain("Token “nightly burn-down” created");
    expect(html).toContain("only time it is shown");
    expect(html).toContain("select-all");
    expect(html).toContain(">Copy</button>");
    expect(html).toContain(">Done</button>");
  });
});
