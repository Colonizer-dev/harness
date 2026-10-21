// What the cockpit says about an issue's intake (issue #1485): who approved it and what the PromptDecode
// scan found. Pure, so the list (IssueList.tsx) and its tests read the same words.
import type { IssueIntake } from "../features/repos/types";

export interface IntakeNote {
  tone: "ok" | "warn" | "muted";
  text: string;
}

/** The line under an issue's title: the approver, or the scan result, or why it waits. Null when there is nothing to say. */
export function intakeNote(intake: IssueIntake | null | undefined): IntakeNote | null {
  if (!intake) return null;
  const scan = intake.scan ?? null;
  const findings = scan && !scan.clean ? scan.findings : [];
  if (intake.reason === "prompt_decode_finding" && scan) {
    const first = findings[0];
    const decoded = first?.decoded ? `, decodes to "${first.decoded}"` : "";
    const count = findings.length + scan.more;
    return { tone: "warn", text: `Needs review: PromptDecode finding (${count})${decoded}` };
  }
  if (intake.approved_by) {
    const scanned = scan ? (scan.clean ? "PromptDecode clean" : `PromptDecode: ${findings.length + scan.more} finding${findings.length + scan.more === 1 ? "" : "s"} shown`) : "";
    return { tone: "ok", text: [`Approved by @${intake.approved_by}`, scanned].filter(Boolean).join(" · ") };
  }
  if (scan) {
    return scan.clean ? { tone: "muted", text: "PromptDecode: clean" } : { tone: "warn", text: `PromptDecode: ${findings.length + scan.more} finding${findings.length + scan.more === 1 ? "" : "s"}` };
  }
  return null;
}
