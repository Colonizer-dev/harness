// Boundary events in the colony timeline (issue #609): a muted row per event, placed after the
// message it followed, and the control-defeat attention item that carries them as evidence.
// Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { textFor } from "../cockpit/feed";
import { END_OF_THREAD, buildThread, initialStreamState, reduceFrame } from "../sessionStream";
import type { BoundaryRecord, Session } from "../types";
import { BoundaryRow, boundaryLabel } from "./BoundaryRow";
import { attentionText } from "./ui";

const egress: BoundaryRecord = {
  type: "boundary",
  kind: "egress_denied",
  control: "egress",
  detail: "Could not resolve host: evil.example",
  target: "evil.example",
  at: "2026-01-01T00:00:00.000Z",
};

describe("BoundaryRow", () => {
  it("is a muted, indented row naming the kind, the target and the control", () => {
    const out = renderToStaticMarkup(<BoundaryRow record={egress} />);
    expect(out).toContain("Network refused");
    expect(out).toContain("evil.example");
    expect(out).toContain("· egress");
    expect(out).toContain("text-faint");
    expect(out).toContain("ml-10");
    expect(out).toContain('data-boundary="egress_denied"');
  });

  it("falls back to the detail when no target was named, and drops the indent as evidence", () => {
    const { target: _target, ...noTarget } = egress;
    const out = renderToStaticMarkup(<BoundaryRow record={noTarget} evidence />);
    expect(out).toContain("Could not resolve host: evil.example");
    expect(out).not.toContain("ml-10");
    expect(out).not.toContain("text-faint");
  });

  it("labels every kind", () => {
    expect(boundaryLabel({ kind: "publish_rewrite_refused" })).toBe("Publish rewrote colony output");
    expect(boundaryLabel({ kind: "exec_policy_ask_bypass_attempt" })).toBe("Asked again after a refusal");
  });
});

describe("boundary events in the stream", () => {
  it("are kept with the message they followed, and replayed lines are not doubled", () => {
    let state = initialStreamState();
    state = reduceFrame(state, { seq: 1, type: "user_message", id: "u1", text: "go" });
    state = reduceFrame(state, { seq: 2, ...egress, ts: "2026-01-01T00:00:01Z", origin: "agent" });
    state = reduceFrame(state, { seq: 2, ...egress });
    expect(state.boundaries).toHaveLength(1);
    expect(state.boundaries[0].record).toEqual(egress);
    expect(state.boundaries[0].afterMessageId).toBe("u1");
    const thread = buildThread(state);
    expect(thread.boundaries.u1?.map((n) => n.record.kind)).toEqual(["egress_denied"]);
    expect(thread.boundaries[END_OF_THREAD]).toBeUndefined();
  });

  it("before any message they sit at the end of the thread", () => {
    const state = reduceFrame(initialStreamState(), { seq: 1, ...egress });
    expect(buildThread(state).boundaries[END_OF_THREAD]).toHaveLength(1);
  });
});

describe("the control-defeat attention item", () => {
  it("names itself in the banner and the feed", () => {
    const attention = {
      reason: "control_defeat" as const,
      since: "2026-01-01T00:00:00Z",
      nudges: 0,
      signature: "deny_then_reach",
      detail: "a tool call reached `evil.example` after `egress` refused it",
      evidence: [egress],
    };
    expect(attentionText(attention)).toBe("A control may have been bypassed");
    const session = { repo: "acme/webshop", issue: 7, status: "running", attention } as unknown as Session;
    expect(textFor(session, "question")).toBe("webshop#7 may have got past one of its controls");
    const rows = renderToStaticMarkup(
      <>
        {attention.evidence.map((record, i) => (
          <BoundaryRow key={i} record={record} evidence />
        ))}
      </>,
    );
    expect(rows).toContain("evil.example");
  });
});
