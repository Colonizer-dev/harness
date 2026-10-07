// The findings inspector reads a colony's finding ledger — append-only record lines, several per
// finding as it moves validated → filed → fix_colony → review → merged — and folds them into one
// chain per title. What matters is that the last line to mention a fact is the truth about it, that
// the references a later line stops repeating (the issue, the pr, the fix colony) survive, and that
// the trail a chain walked is exactly the set of lines in it, oldest first.
import { describe, expect, it } from "vitest";

import { chains } from "./findings";
import type { FindingRecord } from "../types";

function line(title: string, overrides: Partial<FindingRecord> & { state: FindingRecord["state"] }): FindingRecord {
  return { session: "demo1234", title, ts: "2026-09-18T09:00:00Z", ...overrides };
}

/** Sequential timestamps, so a test can say the order without spelling out the strings. */
const at = (n: number) => `2026-09-18T09:${String(n).padStart(2, "0")}:00Z`;

describe("chains", () => {
  it("folds a finding that ran the whole way into one chain, lines in order", () => {
    const records = [
      line("Guest checkout swallows the retry hint", { state: "validated", severity: "high", ts: at(0) }),
      line("Guest checkout swallows the retry hint", { state: "filed", issue: "https://github.com/acme/webshop/issues/88", ts: at(1) }),
      line("Guest checkout swallows the retry hint", { state: "fix_colony", fix_session: "fix0001", ts: at(2) }),
      line("Guest checkout swallows the retry hint", { state: "review", review_session: "rev0001", verdict: "pass", ts: at(3) }),
      line("Guest checkout swallows the retry hint", { state: "merged", pr: "https://github.com/acme/webshop/pull/231", ts: at(4) }),
    ];
    const [chain] = chains(records);
    expect(chain).toMatchObject({
      title: "Guest checkout swallows the retry hint",
      state: "merged",
      severity: "high",
      issue: "https://github.com/acme/webshop/issues/88",
      fix_session: "fix0001",
      review_session: "rev0001",
      verdict: "pass",
      pr: "https://github.com/acme/webshop/pull/231",
    });
    expect(chain.records.map((r) => r.state)).toEqual(["validated", "filed", "fix_colony", "review", "merged"]);
  });

  it("the present is the last line's state, not a summary of the trail", () => {
    const records = [
      line("Sign-in badge leaks the session token", { state: "validated", severity: "medium", ts: at(0) }),
      line("Sign-in badge leaks the session token", { state: "filed", issue: "https://github.com/acme/webshop/issues/90", ts: at(1) }),
      line("Sign-in badge leaks the session token", { state: "review", review_session: "rev0003", verdict: "fail", ts: at(2) }),
      line("Sign-in badge leaks the session token", { state: "fix_colony", fix_session: "fix0009", ts: at(3) }),
    ];
    const [chain] = chains(records);
    expect(chain.state).toBe("fix_colony");
    // A later line that omits a fact does not un-say it; the review's verdict is still known.
    expect(chain.verdict).toBe("fail");
    expect(chain.issue).toBe("https://github.com/acme/webshop/issues/90");
  });

  it("keeps the issue and pr that later lines stopped repeating", () => {
    const records = [
      line("Open colony opens the wrong checkout branch", { state: "validated", ts: at(0) }),
      line("Open colony opens the wrong checkout branch", { state: "filed", issue: "#91", ts: at(1) }),
      line("Open colony opens the wrong checkout branch", { state: "merged", pr: "https://github.com/acme/webshop/pull/240", ts: at(2) }),
    ];
    const [chain] = chains(records);
    expect(chain.state).toBe("merged");
    // The merged line never mentioned the issue again; the fold keeps the last word on it.
    expect(chain.issue).toBe("#91");
    expect(chain.pr).toBe("https://github.com/acme/webshop/pull/240");
  });

  it("a rejected finding stops at its reason", () => {
    const records = [
      line("Khaki pad in the order summary", { state: "validated", severity: "low", ts: at(0) }),
      line("Khaki pad in the order summary", { state: "rejected", reason: "already fixed on main", ts: at(1) }),
    ];
    const [chain] = chains(records);
    expect(chain.state).toBe("rejected");
    expect(chain.reason).toBe("already fixed on main");
    expect(chain.records.map((r) => r.state)).toEqual(["validated", "rejected"]);
  });

  it("a legacy duplicate line still lands", () => {
    // A duplicate written by an older mothership: the line names the finding it duplicates, and
    // neither that reference nor the issue it was first filed as gets lost.
    const records = [
      line("Missing alt text on the dashboard charts", { state: "filed", issue: "https://github.com/acme/webshop/issues/81", ts: at(0) }),
      line("Missing alt text on the dashboard charts", { state: "duplicate", duplicate_of: "https://github.com/acme/webshop/issues/79", ts: at(1) }),
    ];
    const [chain] = chains(records);
    expect(chain.state).toBe("duplicate");
    expect(chain.duplicate_of).toBe("https://github.com/acme/webshop/issues/79");
    expect(chain.issue).toBe("https://github.com/acme/webshop/issues/81");
  });

  it("severity follows the latest line that names it", () => {
    const records = [
      line("Voucher code reveals the issuer email", { state: "validated", severity: "low", ts: at(0) }),
      line("Voucher code reveals the issuer email", { state: "validated", severity: "high", ts: at(1) }),
    ];
    expect(chains(records)[0].severity).toBe("high");
  });

  it("keeps chains in first-appearance order and lines in ledger order", () => {
    const records = [
      line("Guest orders show the wrong tax region", { state: "validated", ts: at(0) }),
      line("Checkout fails for guest users", { state: "validated", ts: at(1) }),
      line("Guest orders show the wrong tax region", { state: "filed", issue: "#21", ts: at(2) }),
    ];
    const [guest, checkout] = chains(records);
    expect(guest.title).toBe("Guest orders show the wrong tax region");
    expect(checkout.title).toBe("Checkout fails for guest users");
    expect(guest.records.map((r) => r.state)).toEqual(["validated", "filed"]);
  });

  it("returns nothing for an empty ledger", () => {
    expect(chains([])).toEqual([]);
  });
});