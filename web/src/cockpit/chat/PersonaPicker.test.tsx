import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { ChatMessage } from "../../types";
import { MessageRow, StreamingRow } from "./Message";
import { PersonaAnt } from "./PersonaAnt";
import { PersonaList, PersonaPicker } from "./PersonaPicker";
import { loadPersonas } from "./logic";

const personas = loadPersonas({});
const sarge = personas.find((p) => p.id === "reviewer")!;

describe("persona picker", () => {
  it("shows the chosen ant's avatar and name on the button", () => {
    const html = renderToStaticMarkup(<PersonaPicker personas={personas} value={sarge} onPick={() => {}} />);
    expect(html).toContain('aria-label="persona"');
    expect(html).toContain("Sarge");
    expect(html).toContain('data-kind="soldier"');
    expect(html).toContain('aria-label="Sarge, soldier ant"');
  });

  it("lists every ant with its name, caste, role and one-line job, the chosen one marked", () => {
    const html = renderToStaticMarkup(<PersonaList personas={personas} selected="reviewer" onPick={() => {}} />);
    for (const p of personas) {
      expect(html).toContain(p.ant);
      expect(html).toContain(p.species);
      expect(html).toContain(p.name);
      expect(html).toContain(p.blurb);
    }
    expect(html.match(/aria-selected="true"/g)).toHaveLength(1);
    expect(html).toMatch(/aria-selected="true" data-persona="reviewer"/);
    // The full prompt stays reachable (folded, and as the card's tooltip); Plain has none to show.
    expect(html).toContain(sarge.system);
    expect(html.match(/Show the prompt/g)).toHaveLength(3);
  });

  it("draws each ant still, breathing or walking", () => {
    for (const motion of ["none", "idle", "active"] as const) {
      expect(renderToStaticMarkup(<PersonaAnt persona={sarge} motion={motion} />)).toContain(`data-motion="${motion}"`);
    }
    const kinds = personas.map((p) => renderToStaticMarkup(<PersonaAnt persona={p} />));
    expect(kinds[0]).toContain("pa-leaf");
    expect(kinds[1]).toContain("pa-mandible");
    expect(kinds[2]).toContain("pa-silk");
    expect(kinds[3]).toContain("pa-drop");
  });

  it("speaks replies as the conversation's ant, and plain ones as the model", () => {
    const m: ChatMessage = { id: "a", role: "assistant", content: "Found a bug.", ts: "2026-09-24T00:00:00Z", model: "claude-opus-5-5", input_tokens: 1, output_tokens: 1, stopped: false };
    const as = (ant: typeof sarge | null) =>
      renderToStaticMarkup(<MessageRow m={m} models={null} claudeIds={[]} isLastReply busy={false} hit={null} onAction={() => {}} ant={ant} />);
    expect(as(sarge)).toContain("Sarge");
    expect(as(sarge)).toContain('data-kind="soldier"');
    expect(as(null)).not.toContain("persona-ant\"");
    expect(renderToStaticMarkup(<StreamingRow model="claude-opus-5-5" text="" models={null} ant={sarge} />)).toContain('data-motion="active"');
  });
});
