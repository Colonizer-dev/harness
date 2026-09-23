// What the inspector's findings section shows, folded out of a colony's finding ledger.
//
// The ledger is append-only: as a finding moves validated → filed (or rejected or duplicate) →
// fix_colony → review → merged (or blocked), the mothership writes a new line, and no earlier line is
// ever rewritten. The browser's job is to say where each finding is *now*, so this folds the lines
// into one chain per title. The last line to mention a field wins, because it is the most recent
// thing the mothership told us about it. Two consequences follow from the ledger being append-only:
//
// - A finding that became a pull request does not stop being one because a later line stops
//   repeating the `pr`, any more than it un-became the issue it was filed as. The references to
//   other things (`issue`, `pr`, the fix and review colonies) are kept once seen.
// - `state` is always the last line's: that is the reading this fold exists to serve.
//
// The endpoints serve the ledger in the order it was written, so each chain's lines keep that order
// as well — re-sorting on the browser could only invent a tie no line recorded.
import type { FindingRecord } from "../types";

export interface FindingChain {
  title: string;
  /** The finding's state right now, which is the last line's. */
  state: FindingRecord["state"];
  severity?: FindingRecord["severity"];
  reason?: FindingRecord["reason"];
  issue?: FindingRecord["issue"];
  duplicate_of?: FindingRecord["duplicate_of"];
  fix_session?: FindingRecord["fix_session"];
  review_session?: FindingRecord["review_session"];
  verdict?: FindingRecord["verdict"];
  pr?: FindingRecord["pr"];
  /** The ledger lines that built this chain, oldest first. */
  records: FindingRecord[];
}

/** Folds a finding ledger into one chain per title, in first-appearance order. */
export function chains(records: FindingRecord[]): FindingChain[] {
  const folded: FindingChain[] = [];
  const byTitle = new Map<string, FindingChain>();
  for (const record of records) {
    let chain = byTitle.get(record.title);
    if (!chain) {
      chain = { title: record.title, state: record.state, records: [] };
      byTitle.set(record.title, chain);
      folded.push(chain);
    }
    // The last line to carry a fact is the truth about it; a line that leaves one out is simply
    // not talking about it, so the once-seen references survive unretracted.
    if (record.severity !== undefined) chain.severity = record.severity;
    if (record.reason !== undefined) chain.reason = record.reason;
    if (record.issue !== undefined) chain.issue = record.issue;
    if (record.duplicate_of !== undefined) chain.duplicate_of = record.duplicate_of;
    if (record.fix_session !== undefined) chain.fix_session = record.fix_session;
    if (record.review_session !== undefined) chain.review_session = record.review_session;
    if (record.verdict !== undefined) chain.verdict = record.verdict;
    if (record.pr !== undefined) chain.pr = record.pr;
    chain.state = record.state;
    chain.records.push(record);
  }
  return folded;
}