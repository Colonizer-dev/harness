// The operator vault's review queue (issue #777): proposals are untrusted colony text, so they are
// shown escaped and never as Markdown or HTML; each names where it would land and where it came
// from; the queue follows the org filter and hides itself when no vault is configured; and accept
// and reject reach the mothership's routes.
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Api } from "../api";
import { ApiContext } from "../context";
import { memoryHttp } from "../features/memory/api";
import type { VaultProposal, VaultProposalListing } from "../types";
import { VaultProposalList } from "./VaultProposals";

const hostile: VaultProposal = {
  id: "v-1",
  path: "web/<script>alert(1)</script>.md",
  title: '<img src=x onerror="alert(1)">Deploy order',
  body: "# Heading\n\n<script>alert('body')</script>\n[link](javascript:alert(1))",
  reason: "<b>because</b>",
  created_at: "2026-10-05T10:00:00Z",
  source: { session_id: "colony-42", repo: "acme/web", commit: "4f2c9e1a7b3d5e6f", origin: "orchestrator" },
};

const listing = (proposals: VaultProposal[], configured = true): VaultProposalListing => ({ configured, inbox: "Inbox/colonizer", proposals });

const render = (node: React.ReactNode) => renderToStaticMarkup(<ApiContext.Provider value={{} as Api}>{node}</ApiContext.Provider>);

describe("vault proposals", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("shows a proposal escaped, with where it lands and where it came from", () => {
    const html = render(<VaultProposalList listing={listing([hostile])} error={null} selectedOrg={null} onResolved={() => {}} />);
    expect(html).toContain("Proposed for your vault");
    expect(html).not.toMatch(/<script|<img|<b>|href="javascript/);
    expect(html).toContain("&lt;img src=x onerror=&quot;alert(1)&quot;&gt;Deploy order");
    expect(html).toContain("&lt;script&gt;alert(&#x27;body&#x27;)&lt;/script&gt;");
    expect(html).toContain("# Heading");
    expect(html).toContain("Inbox/colonizer/web/&lt;script&gt;alert(1)&lt;/script&gt;.md");
    expect(html).toContain("colony-42");
    expect(html).toContain("acme/web");
    expect(html).toContain("4f2c9e1a7b3d");
    expect(html).not.toContain("4f2c9e1a7b3d5e6f");
    expect(html).toContain("Accept into vault");
    expect(html).toContain("Reject");
  });

  it("follows the org filter, and hides itself when no vault is configured and nothing waits", () => {
    const other = render(<VaultProposalList listing={listing([hostile])} error={null} selectedOrg="globex" onResolved={() => {}} />);
    expect(other).toContain("No vault proposals waiting.");
    expect(other).not.toContain("colony-42");
    const same = render(<VaultProposalList listing={listing([hostile])} error={null} selectedOrg="ACME" onResolved={() => {}} />);
    expect(same).toContain("colony-42");
    expect(render(<VaultProposalList listing={listing([], false)} error={null} selectedOrg={null} onResolved={() => {}} />)).toBe("");
    expect(render(<VaultProposalList listing={null} error={null} selectedOrg={null} onResolved={() => {}} />)).toBe("");
    expect(render(<VaultProposalList listing={null} error="mothership down" selectedOrg={null} onResolved={() => {}} />)).toContain("mothership down");
  });

  it("lists, accepts and rejects through the mothership's vault routes", async () => {
    const calls: { url: string; method: string }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit = {}) => {
        calls.push({ url, method: init.method ?? "GET" });
        const body = url.endsWith("/accept") ? { ok: true, path: "Inbox/colonizer/web/n.md" } : url.endsWith("/reject") ? { ok: true } : listing([hostile]);
        return new Response(JSON.stringify(body), { status: 200 });
      }),
    );
    expect((await memoryHttp.vaultProposals()).proposals[0].id).toBe("v-1");
    expect(await memoryHttp.acceptVaultProposal("v/1")).toEqual({ ok: true, path: "Inbox/colonizer/web/n.md" });
    await memoryHttp.rejectVaultProposal("v-2");
    expect(calls).toEqual([
      { url: "/api/vault/proposals", method: "GET" },
      { url: "/api/vault/proposals/v%2F1/accept", method: "POST" },
      { url: "/api/vault/proposals/v-2/reject", method: "POST" },
    ]);
  });
});
