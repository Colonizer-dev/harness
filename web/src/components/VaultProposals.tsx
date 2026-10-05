import { useCallback, useEffect, useState } from "react";
import { errorMessage, useApi, useToast } from "../context";
import type { VaultProposal, VaultProposalListing } from "../types";
import { IconCheck, IconTrash } from "./icons";
import { Badge, Button, Spinner, sameOrg, timeAgo } from "./ui";

/**
 * Notes colonies proposed for the operator vault (issue #777), beside the memory review queue.
 * Accept writes one new file into the vault's inbox folder; reject drops the proposal. Every field
 * but the provenance came from a colony, so it is shown as plain, escaped text — never Markdown.
 * Renders nothing when no vault is configured and nothing is queued.
 */
export function VaultProposals({ selectedOrg }: { selectedOrg: string | null }) {
  const api = useApi();
  const [listing, setListing] = useState<VaultProposalListing | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setListing(await api.vaultProposals());
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
    const timer = setInterval(load, 10_000);
    return () => clearInterval(timer);
  }, [load]);

  const resolved = (id: string) =>
    setListing((current) => (current ? { ...current, proposals: current.proposals.filter((p) => p.id !== id) } : current));

  return <VaultProposalList listing={listing} error={error} selectedOrg={selectedOrg} onResolved={resolved} />;
}

/** The section itself, stateless so it renders in a test without the API. */
export function VaultProposalList({
  listing,
  error,
  selectedOrg,
  onResolved,
}: {
  listing: VaultProposalListing | null;
  error: string | null;
  selectedOrg: string | null;
  onResolved: (id: string) => void;
}) {
  if (!listing && !error) return null;
  if (listing && !listing.configured && listing.proposals.length === 0) return null;
  const visible = listing?.proposals.filter((p) => !selectedOrg || sameOrg(p.source.repo.split("/")[0], selectedOrg)) ?? [];
  return (
    <section aria-labelledby="vault-proposals-title" className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id="vault-proposals-title" className="text-lead-sm font-semibold">
          Proposed for your vault
        </h2>
        {visible.length > 0 && <Badge tone="accent">{visible.length}</Badge>}
      </div>
      {error && <p className="text-body-sm text-err">{error}</p>}
      {listing && (
        <p className="text-small-lg text-muted">
          Accepting writes a new note into <code className="font-mono text-small text-text">{listing.inbox}/</code> in your vault. Nothing is
          overwritten, and nothing reaches the vault until you accept it.
        </p>
      )}
      {listing && visible.length === 0 && (
        <p className="rounded-xl border border-dashed border-border-strong px-4 py-5 text-center text-body-sm text-muted">No vault proposals waiting.</p>
      )}
      {listing &&
        visible.map((proposal) => (
          <VaultProposalCard key={proposal.id} proposal={proposal} inbox={listing.inbox} onResolved={onResolved} />
        ))}
    </section>
  );
}

function VaultProposalCard({ proposal, inbox, onResolved }: { proposal: VaultProposal; inbox: string; onResolved: (id: string) => void }) {
  const api = useApi();
  const toast = useToast();
  const [busy, setBusy] = useState<"accept" | "reject" | null>(null);

  const run = async (kind: "accept" | "reject") => {
    setBusy(kind);
    try {
      if (kind === "accept") {
        const { path } = await api.acceptVaultProposal(proposal.id);
        toast(`Wrote ${path} to your vault`);
      } else {
        await api.rejectVaultProposal(proposal.id);
        toast("Vault proposal rejected");
      }
      onResolved(proposal.id);
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  const commit = proposal.source.commit ? proposal.source.commit.slice(0, 12) : null;
  return (
    <article className="rounded-xl border border-border bg-panel">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-4 pt-3 text-small-lg text-muted">
        <Badge>
          <span className="font-mono font-medium [overflow-wrap:anywhere]">
            {inbox}/{proposal.path}
          </span>
        </Badge>
        <span className="text-faint">· {timeAgo(proposal.created_at)}</span>
      </div>
      <div className="space-y-2 px-4 py-3">
        <h3 className="text-lead-sm font-semibold [overflow-wrap:anywhere]">{proposal.title}</h3>
        <pre className="max-h-72 overflow-auto whitespace-pre-wrap rounded-lg bg-panel-2 px-3 py-2 font-mono text-small-lg leading-relaxed text-text [overflow-wrap:anywhere]">
          {proposal.body}
        </pre>
        <p className="text-small-lg text-muted [overflow-wrap:anywhere]">
          <span className="font-medium text-text">Why:</span> {proposal.reason}
        </p>
        <p className="text-small text-faint [overflow-wrap:anywhere]">
          From colony <code className="font-mono">{proposal.source.session_id}</code> on <span className="font-mono">{proposal.source.repo}</span>
          {commit && (
            <>
              {" "}
              at <code className="font-mono">{commit}</code>
            </>
          )}
        </p>
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-4 py-2.5">
        <Button size="sm" variant="danger" className="mr-auto" disabled={busy !== null} onClick={() => run("reject")}>
          {busy === "reject" ? <Spinner /> : <IconTrash size={13} />} Reject
        </Button>
        <Button size="sm" variant="primary" disabled={busy !== null} onClick={() => run("accept")}>
          {busy === "accept" ? <Spinner /> : <IconCheck size={13} />} Accept into vault
        </Button>
      </div>
    </article>
  );
}
