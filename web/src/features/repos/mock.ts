// The `repos` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { ago, now, sleep } from "../../mockShared";
import type { ArchMap, Issue, IssueDrafts, PackagesDependencies, PackagesPublished, Repo, SupplyChain } from "../../types";
import type { MockState } from "../../mockState";
import type { ReposApi } from "./api";

export const REPOS: Repo[] = [
  { full_name: "acme/webshop", description: "Storefront and checkout", private: true, fork: false, archived: false, open_issues_count: 12, pushed_at: ago(30) },
  { full_name: "acme/design-system", description: "Shared UI components", private: false, fork: false, archived: false, open_issues_count: 5, pushed_at: ago(600) },
  { full_name: "octocat/hello-world", description: null, private: false, fork: true, archived: false, open_issues_count: 0, pushed_at: ago(9000) },
];

export const ISSUES: Record<string, Issue[]> = {
  "acme/webshop": [
    {
      number: 42,
      title: "Checkout fails for guest users",
      body: 'Guests get "Something went wrong" when pressing **Pay**. Logged-in users are fine.\n\nSteps:\n1. Open the shop in a private window\n2. Add any item\n3. Checkout without signing in',
      labels: [
        { name: "bug", color: "d73a4a" },
        { name: "checkout", color: "0e8a16" },
      ],
      author: { login: "maria" },
      updatedAt: ago(90),
      url: "https://github.com/acme/webshop/issues/42",
    },
    {
      number: 43,
      title: "Add dark mode to the order confirmation email",
      body: "The confirmation email is unreadable in dark-mode mail clients.",
      labels: [{ name: "enhancement", color: "a2eeef" }],
      author: { login: "sam" },
      updatedAt: ago(1500),
      url: "https://github.com/acme/webshop/issues/43",
    },
    {
      number: 44,
      title: "Search results ignore the size filter",
      body: null,
      labels: [{ name: "bug", color: "d73a4a" }, { name: "search", color: "1d76db" }],
      author: { login: "noor" },
      updatedAt: ago(200),
      url: "https://github.com/acme/webshop/issues/44",
    },
    {
      number: 45,
      title: "Show stock levels on the product page",
      body: null,
      labels: [{ name: "enhancement", color: "a2eeef" }],
      author: { login: "sam" },
      updatedAt: ago(400),
      url: "https://github.com/acme/webshop/issues/45",
    },
    {
      number: 46,
      title: "Cart badge does not update after removing the last item",
      body: null,
      labels: [{ name: "bug", color: "d73a4a" }],
      author: { login: "maria" },
      updatedAt: ago(700),
      url: "https://github.com/acme/webshop/issues/46",
    },
    {
      number: 47,
      title: "Add Apple Pay to checkout",
      body: null,
      labels: [{ name: "checkout", color: "0e8a16" }, { name: "enhancement", color: "a2eeef" }],
      author: { login: "lee" },
      updatedAt: ago(900),
      url: "https://github.com/acme/webshop/issues/47",
    },
    {
      number: 48,
      title: "Product images load at full resolution on mobile",
      body: null,
      labels: [{ name: "performance", color: "fbca04" }],
      author: { login: "noor" },
      updatedAt: ago(1100),
      url: "https://github.com/acme/webshop/issues/48",
    },
    {
      number: 49,
      title: "Wishlist loses items after signing out",
      body: null,
      labels: [{ name: "bug", color: "d73a4a" }],
      author: { login: "sam" },
      updatedAt: ago(1300),
      url: "https://github.com/acme/webshop/issues/49",
    },
    {
      number: 50,
      title: "Translate the footer into German",
      body: null,
      labels: [{ name: "i18n", color: "c5def5" }],
      author: { login: "maria" },
      updatedAt: ago(1700),
      url: "https://github.com/acme/webshop/issues/50",
    },
    {
      number: 51,
      title: "Order history pagination skips page 2",
      body: null,
      labels: [{ name: "bug", color: "d73a4a" }],
      author: { login: "lee" },
      updatedAt: ago(2100),
      url: "https://github.com/acme/webshop/issues/51",
    },
    {
      number: 52,
      title: "Rate-limit the newsletter signup form",
      body: null,
      labels: [{ name: "security", color: "b60205" }],
      author: { login: "noor" },
      updatedAt: ago(2500),
      url: "https://github.com/acme/webshop/issues/52",
    },
    {
      number: 53,
      title: "Add a sitemap.xml for the catalogue",
      body: null,
      labels: [{ name: "seo", color: "bfd4f2" }],
      author: { login: "sam" },
      updatedAt: ago(3200),
      url: "https://github.com/acme/webshop/issues/53",
    },
  ],
  "acme/design-system": [
    {
      number: 7,
      title: "Button focus ring is invisible on dark backgrounds",
      body: null,
      labels: [{ name: "a11y", color: "5319e7" }],
      author: { login: "lee" },
      updatedAt: ago(300),
      url: "https://github.com/acme/design-system/issues/7",
    },
  ],
};
/** `"Ready, colonize ,ready"` → `["Ready", "colonize"]`: the mothership's split_labels. */
export function mockSplitLabels(raw: string): string[] {
  const out: string[] = [];
  for (const l of raw.split(",").map((x) => x.trim()).filter(Boolean)) if (!out.some((o) => o.toLowerCase() === l.toLowerCase())) out.push(l);
  return out;
}

/** The mock's summary model for Colonize: each bullet or numbered line an issue, else the text as one. */
export function mockDrafts(text: string): IssueDrafts {
  const items = text
    .split("\n")
    .map((l) => l.match(/^\s*(?:[-*•]|\d+[.)])\s+(.+)$/)?.[1]?.trim())
    .filter((l): l is string => Boolean(l));
  const tasks = items.length > 1 ? items.slice(0, 5) : [text.trim()];
  const title = (t: string) => {
    const line = t.split(/[.\n]/)[0].trim().replace(/^please\s+/i, "");
    const cut = line.length > 72 ? `${line.slice(0, 71).trimEnd()}…` : line;
    return cut.charAt(0).toUpperCase() + cut.slice(1);
  };
  return {
    issues: tasks.map((t) => ({
      title: title(t),
      body: `${t}\n\n### Done when\n- The change is covered by a test\n- The existing suite still passes`,
    })),
    model: "mock/summary",
  };
}
/** A demo architecture map (the shape GET /api/maps returns) for the mock's main repository. */
export const DEMO_MAP: ArchMap = {
  title: "acme/webshop",
  subtitle: "storefront · checkout · webhooks",
  components: [
    { id: "web", type: "frontend", label: "Storefront", sublabel: "Next.js", pos: [0, 0], size: [160, 60], sources: [{ path: "apps/web/src/app/layout.tsx" }] },
    { id: "api", type: "backend", label: "API gateway", sublabel: "routes · auth", pos: [260, 0], size: [170, 60], sources: [{ path: "services/api/src/server.ts" }] },
    { id: "checkout", type: "backend", label: "Checkout", sublabel: "cart · payments", pos: [120, 170], size: [170, 64], sources: [{ path: "services/checkout/src/checkout.ts" }] },
    { id: "email", type: "backend", label: "Email", sublabel: "templates", pos: [420, 170], size: [140, 56], sources: [{ path: "services/email/templates/order.mjml" }] },
    { id: "webhooks", type: "backend", label: "Webhooks", sublabel: "retries · backoff", pos: [260, 330], size: [160, 60], sources: [{ path: "services/webhooks/src/deliver.ts" }] },
    { id: "db", type: "database", label: "Postgres", pos: [0, 330], size: [140, 60], sources: [{ path: "db/migrations/0001_init.sql" }] },
    { id: "stripe", type: "external", label: "Stripe", pos: [560, 330], size: [120, 56], sources: [] },
  ],
  connections: [
    { from: "web", to: "api" },
    { from: "api", to: "checkout" },
    { from: "api", to: "email" },
    { from: "checkout", to: "db" },
    { from: "checkout", to: "webhooks" },
    { from: "webhooks", to: "stripe" },
    { from: "email", to: "webhooks" },
  ],
  boundaries: [
    { label: "services", wraps: ["api", "checkout", "email", "webhooks"] },
  ],
};

export function reposMock(ms: MockState): ReposApi {
  return {
    repos: () => ms.later(() => REPOS, 350),
    issues: (repo) => ms.later(() => ISSUES[repo] ?? [], 300),
    draftIssues: (body) => ms.later(() => ({ ...mockDrafts(body.text), labels: ms.sourceLabels() }), 900),
    createIssue: async (repo, body) => {
      await sleep(500);
      const list = (ISSUES[repo] ??= []);
      const number = Math.max(99, ...Object.values(ISSUES).flat().map((i) => i.number)) + 1;
      const url = `https://github.com/${repo}/issues/${number}`;
      const wanted = ms.sourceLabels();
      const labels = wanted.filter((l) => !/^locked/i.test(l));
      const labels_skipped = wanted.filter((l) => !labels.includes(l));
      list.unshift({ number, title: body.title, body: body.body, labels: labels.map((name) => ({ name, color: "c5def5" })), author: { login: "octocat" }, updatedAt: now(), url });
      ms.logActivity({ kind: "colonize.issue", actor: "you", via: "cockpit", org: repo.split("/")[0], repo, issue: number, title: body.title });
      return { repo, number, title: body.title, url, labels, labels_skipped };
    },
    repoPackages: (repo) =>
      ms.later(
        () =>
          repo.endsWith("/web")
            ? { monorepo: true, tool: "turbo", packages: [{ name: "pwa", path: "apps/pwa" }, { name: "website", path: "apps/website" }, { name: "sdk", path: "packages/sdk" }] }
            : { monorepo: false, tool: null, packages: [] },
        200,
      ),
    repoMap: (repo) => ms.later(() => ms.repoMap(repo)),
    repoLoc: () =>
      ms.later(() => ({
        ref: "main",
        sha: "a".repeat(40),
        total: 48210,
        by_language: [
          { name: "Rust", files: 88, code: 31200, blank: 3100 },
          { name: "TSX", files: 120, code: 12400, blank: 1200 },
          { name: "TypeScript", files: 40, code: 3610, blank: 300 },
          { name: "Shell", files: 12, code: 1000, blank: 90 },
        ],
      })),
    repoCoverage: () => ms.later(() => ({ measured: false as const, reason: "no coverage report found in CI artifacts" })),
    repoGitSummary: (repo) => ms.later(() => ({ repo, branches: 14, open_prs: 3, release: { tagName: "v0.1.9", name: "v0.1.9", publishedAt: "2026-09-24T14:00:00Z" }, latest_tag: null })),
    repoBranches: (repo) =>
      ms.later(() => ({
        repo,
        default: "main",
        branches: [
          { name: "main", sha: "a".repeat(40), date: "2026-09-24T14:00:00Z", author: "Ann", message: "Release v0.1.9", default: true, protected: true, colony: false, ahead: 0, behind: 0, pr: null },
          { name: "colonizer/issue-12-ab", sha: "b".repeat(40), date: "2026-09-24T13:00:00Z", author: "Colony", message: "Fix the thing", default: false, protected: false, colony: true, ahead: 2, behind: 1, pr: { number: 501, title: "Fix the thing", url: "https://github.com/x/y/pull/501", isDraft: false } },
        ],
      })),
    repoTree: (repo, ref) => ms.later(() => ({ repo, ref: ref ?? "main", sha: "a".repeat(40), paths: ["README.md", "src/main.rs", "src/lib.rs", "web/src/App.tsx"], truncated: false })),
    repoBlob: (_repo, path, ref) => ms.later(() => ({ path, ref: ref ?? "main", sha: "a".repeat(40), size: 40, binary: false, too_large: false, text: `// ${path}\nfn main() {\n    println!("hello");\n}\n` })),
    fileHistory: (_repo, path, ref) => ms.later(() => ({ path, ref: ref ?? "main", commits: [{ sha: "a".repeat(40), author: "Ann", date: "2026-09-24T10:00:00Z", message: "Add main" }] })),
    fileBlame: (_repo, path, ref) => ms.later(() => ({ path, ref: ref ?? "main", sha: "a".repeat(40), commits: { ["a".repeat(40)]: { author: "Ann", time: 1790000000, summary: "Add main" } }, lines: Array(4).fill("a".repeat(40)) })),
    createEdits: async (repo, body) => ({ url: `https://github.com/${repo}/pull/999`, branch: body.branch, base: body.base ?? "main" }),
    askFile: async (_repo, body) => ({ answer: `*(mock)* \`${body.path}\` answers: ${body.question}`, model: "mock/model" }),
    drafts: async (repo) => ({ repo, autosave: true, drafts: [] }),
    saveDraft: async () => ({ saved_at: new Date().toISOString() }),
    deleteDrafts: async () => ({ removed: 0 }),
    editorSettings: async () => ({ autosave: true }),
    saveEditorSettings: async (body) => body,
    orgPublished: (org) =>
      ms.later(
        (): PackagesPublished => ({
          org,
          scanned_at: new Date().toISOString(),
          repos: [{ repo: `${org}/web`, sha: "abc1234", defined: 3 }],
          packages: [
            { ecosystem: "npm", name: `@${org.toLowerCase()}/sdk`, version: "1.4.0", repo: `${org}/web`, path: "packages/sdk", private: false, registry: null, status: "published", unreleased_changes: true, published: { latest: "1.3.2", published_at: "2026-09-20T10:00:00Z", downloads: 1240, downloads_period: "last week", url: "https://www.npmjs.com/" } },
            { ecosystem: "npm", name: "pwa", version: "0.0.0", repo: `${org}/web`, path: "apps/pwa", private: true, registry: null, status: "private", unreleased_changes: false, published: null },
            { ecosystem: "cargo", name: "colonizer-harness", version: "0.1.9", repo: `${org}/harness`, path: "crates/colonizer", private: false, registry: null, status: "published", unreleased_changes: false, published: { latest: "0.1.9", published_at: "2026-09-24T13:59:00Z", downloads: 310, downloads_period: "90 days", url: "https://crates.io/" } },
            // Enough workspace packages to page through (the dashboard shows ten at a time).
            ...Array.from({ length: 48 }, (_, i): PackagesPublished["packages"][number] => {
              const status = (["published", "unpublished", "private"] as const)[i % 3];
              const repo = `${org}/${["web", "app", "design-system"][i % 3]}`;
              return { ecosystem: i % 7 === 0 ? "cargo" : "npm", name: `@${org.toLowerCase()}/pkg-${String(i + 1).padStart(2, "0")}`, version: `0.${i % 5}.${i % 4}`, repo, path: `packages/pkg-${i + 1}`, private: status === "private", registry: null, status, unreleased_changes: i % 4 === 0 && status === "published", published: status === "published" ? { latest: `0.${i % 5}.0`, published_at: ago(60 * (i + 1) * 9), downloads: 40 * (i + 3), downloads_period: "last week", url: "https://www.npmjs.com/" } : null };
            }),
          ],
          github_packages: { packages: [{ name: "mothership", type: "container", visibility: "private", versions: 12, updated_at: "2026-09-23T08:00:00Z", url: null, repo: `${org}/harness` }], note: null },
        }),
        300,
      ),
    orgDependencies: (org) =>
      ms.later(
        (): PackagesDependencies => ({
          org,
          scanned_at: new Date().toISOString(),
          repos: [{ repo: `${org}/web`, sha: "abc1234", lockfiles: ["bun.lock"] }],
          ecosystems: [
            { ecosystem: "npm", direct: 17, transitive: 49 },
            { ecosystem: "cargo", direct: 1, transitive: 0 },
          ],
          totals: { direct: 18, transitive: 49, outdated: 14, vulnerable: 1 },
          packages: [
            { ecosystem: "npm", name: "react", direct: true, dev: false, latest: "19.1.1", outdated: true, vulnerable: false, drift: true, versions: [{ version: "19.1.0", behind: true, users: [{ repo: `${org}/web`, path: "bun.lock" }], vulns: [] }, { version: "18.3.1", behind: true, users: [{ repo: `${org}/app`, path: "package-lock.json" }], vulns: [] }] },
            { ecosystem: "npm", name: "lodash", direct: false, dev: false, latest: null, outdated: false, vulnerable: true, drift: false, versions: [{ version: "4.17.20", behind: false, users: [{ repo: `${org}/web`, path: "bun.lock" }], vulns: [{ id: "GHSA-35jh-r3h4-6jhm", summary: "Command injection in lodash", severity: "high", fixed: "4.17.21", url: "https://osv.dev/vulnerability/GHSA-35jh-r3h4-6jhm" }] }] },
            { ecosystem: "cargo", name: "serde", direct: true, dev: false, latest: "1.0.210", outdated: false, vulnerable: false, drift: false, versions: [{ version: "1.0.210", behind: false, users: [{ repo: `${org}/harness`, path: "Cargo.lock" }], vulns: [] }] },
            ...Array.from({ length: 64 }, (_, i): PackagesDependencies["packages"][number] => ({ ecosystem: "npm", name: `dep-${String(i + 1).padStart(2, "0")}`, direct: i % 4 === 0, dev: i % 8 === 0, latest: i % 5 === 0 ? "2.0.0" : null, outdated: i % 5 === 0, vulnerable: false, drift: false, versions: [{ version: "1.0.0", behind: i % 5 === 0, users: [{ repo: `${org}/${i % 2 ? "web" : "app"}`, path: i % 2 ? "bun.lock" : "package-lock.json" }], vulns: [] }] })),
          ],
        }),
        300,
      ),
    orgSupplyChain: (org) =>
      ms.later(
        (): SupplyChain => ({
          org,
          scanned_at: new Date().toISOString(),
          repos: [{ repo: `${org}/web`, sha: "abc1234", lockfiles: ["bun.lock"] }],
          counts: { high: 2, moderate: 9, low: 15 },
          fixable: 1,
          risks: [
            { severity: "high", kind: "vulnerability", ecosystem: "npm", name: "lodash", version: "4.17.20", reason: "GHSA-35jh-r3h4-6jhm: Command injection in lodash", fix: { available: true, version: "4.17.21" }, url: "https://osv.dev/vulnerability/GHSA-35jh-r3h4-6jhm", direct: false, via: ["some-lib"], users: [{ repo: `${org}/web`, path: "bun.lock" }] },
            { severity: "high", kind: "typosquat", ecosystem: "npm", name: "expres", version: "1.0.0", reason: "name is one or two letters from the popular \"express\" — check it is the package you meant", fix: { available: false }, url: "https://www.npmjs.com/package/expres", direct: true, via: [], users: [{ repo: `${org}/web`, path: "package-lock.json" }] },
            { severity: "moderate", kind: "install-script", ecosystem: "npm", name: "esbuild", version: "0.23.0", reason: "runs code on install — review: postinstall: node install.js", fix: { available: false }, url: "https://www.npmjs.com/package/esbuild", direct: true, via: [], users: [{ repo: `${org}/web`, path: "bun.lock" }] },
            { severity: "low", kind: "missing-integrity", ecosystem: "npm", name: "left-pad", version: null, reason: "no integrity hash in package-lock.json", fix: { available: false }, url: "", direct: true, via: [], users: [{ repo: `${org}/app`, path: "package-lock.json" }] },
            ...Array.from({ length: 22 }, (_, i): SupplyChain["risks"][number] => ({ severity: i % 3 === 0 ? "moderate" : "low", kind: i % 2 ? "unpinned-version" : "fresh-release", ecosystem: i % 5 === 0 ? "cargo" : "npm", name: `risky-${String(i + 1).padStart(2, "0")}`, version: "0.1.0", reason: i % 2 ? "a range that accepts any future version" : "published under 48 hours ago", fix: { available: false }, url: "", direct: true, via: [], users: [{ repo: `${org}/${i % 2 ? "web" : "harness"}`, path: i % 5 === 0 ? "Cargo.lock" : "bun.lock" }] })),
          ],
          note: "registry facts for up to 300 direct or vulnerable versions; advisories from OSV.dev",
        }),
        300,
      ),
    repoMeta: (repo) =>
      ms.later(() => ({
        full_name: repo,
        description: `The ${repo.split("/")[1]} repository`,
        homepage: null,
        stars: 12,
        primary_language: "Rust",
        languages: [
          { name: "Rust", bytes: 7000, percent: 70 },
          { name: "TypeScript", bytes: 2500, percent: 25 },
          { name: "Shell", bytes: 500, percent: 5 },
        ],
        commits_weekly: Array.from({ length: 52 }, (_, i) => (i * 7) % 13),
        stats_pending: false,
        contributors: [
          { login: "ada", avatar_url: "", contributions: 120 },
          { login: "linus", avatar_url: "", contributions: 40 },
        ],
        pushed_at: new Date().toISOString(),
        html_url: `https://github.com/${repo}`,
      })),
    repoMapFile: (repo, path) =>
      ms.later(() => ({
        repo,
        path,
        colonies: [
          {
            id: "mock-colony",
            title: "Harden the gateway budget",
            issue: 409,
            status: "running" as const,
            mode: "changing" as const,
            activity: [
              { ts: new Date(Date.now() - 40_000).toISOString(), tool: "Edit", summary: "Edit (\u22123 +7)", agent: "Builder Settler" },
              { ts: new Date(Date.now() - 95_000).toISOString(), tool: "Read", summary: `Read ${path.split("/").pop()}:120-180`, agent: null },
            ],
            diff: `diff --git a/${path} b/${path}\n--- a/${path}\n+++ b/${path}\n@@ -10,4 +10,6 @@ fn reserve()\n     let a = 1;\n-    let b = 2;\n+    let b = 3;\n+    let c = 4;\n+    let d = 5;\n     done();\n`,
            diff_truncated: false,
          },
        ],
      })),
    repoMapFiles: (repo) =>
      ms.later(() => {
        const map = ms.repoMap(repo).map;
        const paths = [...new Set((map?.map.components ?? []).flatMap((c) => c.sources.map((s) => s.path)).concat(["README.md", "Cargo.toml"]))];
        return { repo, revision: map?.revision ?? "HEAD", paths, truncated: false };
      }),
    mapRepo: async (repo) => {
      await sleep(250);
      if (!ms.maps.has(repo) && !ms.mappings.has(repo)) {
        ms.mappings.set(repo, { id: `map_${repo.replace(/\W/g, "")}`, status: "running", created_at: new Date().toISOString() });
        setTimeout(() => {
          ms.maps.set(repo, { ...DEMO_MAP, title: repo });
          const m = ms.mappings.get(repo);
          if (m) m.status = "no_changes";
        }, 6000);
      }
      return ms.repoMap(repo);
    },
    touched: () =>
      ms.later(() => ({
        sessions: {
          demo1234: ["services/checkout/src/guest.ts", "services/checkout/src/checkout.ts", "apps/web/src/app/checkout/page.tsx"],
          stall5678: ["services/email/templates/order-dark.mjml"],
          burn_a1b2c3: ["services/webhooks/src/retry.ts", "services/checkout/src/retry.ts"],
        },
        reading: {
          demo1234: ["services/checkout/src/guest.ts"],
          stall5678: ["services/email/templates/order-dark.mjml"],
        },
      }))
  };
}
