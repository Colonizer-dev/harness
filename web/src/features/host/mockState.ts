// The mock's per-call state slice for the host feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { ArchiveEntry, LoginView } from "../../types";
import { ago, ahead } from "../../mockShared";
import type { MockState } from "../../mockState";

export type HostMockState = {
    archiveEntries: ArchiveEntry[];
    githubSource: string;
    claude: {
        configured: boolean;
        source: string | null;
        kind: string | null;
        account?: string | null;
        account_note?: string | null;
        saved_at?: string | null;
        expires_at?: string | null;
        expires_estimated?: boolean;
    };
    login: LoginView;
};

export function installHostMockState(ms: MockState): void {
  // The log archive (GET /api/archive, issue #496): two seeded bundles. Deletes append to it and
  // retention passes take from it, so the Storage panel's Log archive section moves like the real one.
  ms.archiveEntries = [
    { session: "old98765", repo: "acme/webshop", issue: 61, title: "Checkout fails for guest users", status: "pr_opened", bundle: "old98765-rev1.tar.zst", bytes: 5_242_880, archived_at: ago(1560), revision: 1 },
    { session: "merge5678", repo: "acme/design-system", issue: 18, title: "Dark mode palette drift", status: "merged", bundle: "merge5678-rev2.tar.zst", bytes: 2_621_440, archived_at: ago(238), revision: 2 },
  ];
  ms.githubSource = "gh CLI login";
  ms.claude = {
    configured: true,
    source: "Claude subscription",
    kind: "CLAUDE_CODE_OAUTH_TOKEN",
    account: null,
    account_note: "Anthropic does not resolve a `claude setup-token` to an account, so the account behind this token cannot be shown.",
    saved_at: ago(52 * 24 * 60),
    expires_at: ahead(313),
    expires_estimated: true,
  };
  ms.login = { state: "idle", url: null, message: null };
}
