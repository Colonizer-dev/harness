// The mock's per-call state slice for the memory feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { Mem0Status, MemoryNote, MemoryProposal, VaultProposal, VoiceStatus } from "../../types";
import { ago } from "../../mockShared";
import type { MockState } from "../../mockState";

export type MemoryMockState = {
    mem0: Mem0Status;
    voiceKeys: Set<string>;
    proposals: MemoryProposal[];
    vaultProposals: VaultProposal[];
    notes: MemoryNote[];
    voiceStatus: () => VoiceStatus;
};

export function installMemoryMockState(ms: MockState): void {
  ms.mem0 = { has_key: false, source: null, active: false };
  ms.voiceKeys = new Set<string>();

  // The operator vault (issue #777): one note a colony proposed for the vault's inbox.
  ms.vaultProposals = [
    {
      id: "vprop-release",
      path: "acme/release-order.md",
      title: "Fold the changelog before tagging",
      body: "Run `node scripts/changelog.mjs fold` before `git tag`, or the release notes miss the fragments.",
      reason: "The last two releases shipped without their notes.",
      created_at: ago(8),
      source: { session_id: "stall5678", repo: "acme/webshop", commit: "4f2c9e1a7b3d", origin: "orchestrator" },
    },
  ];

  // Shared memory: three proposals waiting for review, two of them from one colony, and a few notes per scope.
  ms.proposals = [
    {
      id: "prop-emails",
      scope: "repo",
      key: "acme/webshop",
      title: "Build email templates with `npm run build:emails`",
      content:
        "Order and account emails live in `emails/*.mjml` and compile to `dist/emails/*.html`.\n\n- Run `npm run build:emails` once after editing; **don't** pass `--watch` in a colony, it never exits\n- Snapshot tests: `npm test -- emails`",
      tags: ["build", "emails"],
      created_at: ago(21),
      source: { session_id: "stall5678", repo: "acme/webshop", origin: "orchestrator" },
      status: "pending",
    },
    {
      id: "prop-dark",
      scope: "repo",
      key: "acme/webshop",
      title: "Dark mode emails need explicit colors",
      content:
        "Email clients ignore `prefers-color-scheme`, so dark mode comes from the palette in `emails/theme.js`:\n\n- Declare `<meta name=\"color-scheme\" content=\"light dark\">` in every template\n- Inline background and text colors; the partials in `emails/partials/` already do",
      tags: ["emails", "dark-mode"],
      created_at: ago(5),
      source: { session_id: "stall5678", repo: "acme/webshop", origin: "orchestrator" },
      status: "pending",
    },
    {
      id: "prop-pnpm",
      scope: "org",
      key: "acme",
      title: "Use pnpm in acme repositories",
      content: "Every acme repository has a `pnpm-lock.yaml`. Use `pnpm install` and `pnpm run <script>`; `npm install` creates a second lockfile that CI rejects.",
      tags: ["tooling"],
      created_at: ago(3),
      source: { session_id: "demo1234", repo: "acme/webshop", origin: "orchestrator" },
      status: "pending",
    },
  ];
  ms.notes = [
    {
      id: "note-g1",
      scope: "global",
      key: "",
      title: "Ask before adding dependencies",
      content: "Prefer the standard library and what the repository already uses. Ask with a choice card before adding a new package.",
      tags: [],
      created_at: ago(8000),
      source: { user: true },
    },
    {
      id: "note-g2",
      scope: "global",
      key: "",
      title: "Commit messages",
      content: "Imperative mood, under 72 characters, no trailing period. Reference the issue in the body, not the subject.",
      tags: ["git"],
      created_at: ago(7000),
      source: { user: true },
    },
    {
      id: "note-o1",
      scope: "org",
      key: "acme",
      title: "Design tokens come from acme/design-system",
      content: "Never hard-code colours. Import tokens from `@acme/tokens`; dark mode values are under `tokens.dark`.",
      tags: ["ui"],
      created_at: ago(2400),
      source: { session_id: "old98765", repo: "acme/webshop" },
    },
    {
      id: "note-o2",
      scope: "org",
      key: "acme",
      title: "Staging deploys",
      content: "Merges to `main` deploy to staging automatically. Production needs a tagged release; colonies should not tag.",
      tags: [],
      created_at: ago(3000),
      source: { user: true },
    },
    {
      id: "note-r1",
      scope: "repo",
      key: "acme/webshop",
      title: "Prices are integer cents",
      content: "Cart and order totals are stored as integer cents (`amount_cents`). Format with `formatPrice()` from `src/money.ts`.",
      tags: ["checkout"],
      created_at: ago(1560),
      source: { session_id: "old98765", repo: "acme/webshop" },
    },
    {
      id: "note-r2",
      scope: "repo",
      key: "acme/webshop",
      title: "Checkout tests need the Stripe mock",
      content: "Start it with `pnpm stripe:mock` before `pnpm test -- checkout`, or the payment tests time out.",
      tags: ["tests"],
      created_at: ago(900),
      source: { user: true },
    },
    {
      id: "note-r3",
      scope: "repo",
      key: "acme/design-system",
      title: "Storybook is the source of truth",
      content: "Every component change needs an updated story; visual tests run against Storybook.",
      tags: [],
      created_at: ago(5000),
      source: { user: true },
    },
  ];

  ms.voiceStatus = (): VoiceStatus => {
    const mod = ms.modules.find((m) => m.kind === "voice");
    const provider = mod && mod.enabled ? mod.provider : "browser";
    const name = mod?.providers.find((p) => p.id === provider)?.name ?? "Browser";
    const browser = provider === "browser";
    const hasKey = ms.voiceKeys.has(provider);
    return {
      provider,
      name,
      model: browser ? "" : String(mod?.settings?.model ?? "gpt-4o-mini-transcribe"),
      language: String(mod?.settings?.language ?? ""),
      configured: browser || hasKey || provider === "openai_compatible",
      has_key: hasKey,
      source: hasKey ? "saved" : null,
      key_optional: provider === "openai_compatible",
      max_seconds: 120,
      max_bytes: 25 * 1024 * 1024,
    };
  };
}
