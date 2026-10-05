import type { BuiltWith, BuiltWithUse } from "../../types";
import { IconExternal } from "../icons";
import { Badge, Spinner, type Tone } from "../ui";
import { Code, Pane } from "./ui";

// ---------------------------------------------------------------------------
// Built with: what this venture is built with, and the page that says so (issue #944).
//
// The data is a fact, not a setting: nothing here is editable, so the pane takes the answer as a
// prop and SettingsBody loads it — the same shape as Runtime, and no hook the markup test cannot
// stand up. `live` and `planned` are two words and two tones, and a planned entry is never dressed
// as a live one: the word is the claim, and the registry's `url` is where a person checks it.
// ---------------------------------------------------------------------------

/** `live` is in use today; `planned` is named for later. The neutral tone keeps a plan from reading as a fault. */
const STATUS_TONE: Record<BuiltWithUse["status"], Tone> = { live: "ok", planned: "neutral" };

function ProvenanceLinks({ builtWith }: { builtWith: BuiltWith }) {
  return (
    <p className="text-small-lg text-muted">
      This list is {builtWith.venture_name}’s (<Code>{builtWith.venture}</Code>){" "}
      <a className="inline-flex items-center gap-1 text-accent hover:underline" href={builtWith.venture_page} target="_blank" rel="noreferrer">
        venture page <IconExternal size={12} />
      </a>
      , read from the{" "}
      <a className="inline-flex items-center gap-1 text-accent hover:underline" href={builtWith.registry} target="_blank" rel="noreferrer">
        stack registry <IconExternal size={12} />
      </a>
      {builtWith.retrieved ? ` and copied on ${builtWith.retrieved}.` : "."}
    </p>
  );
}

export function BuiltWithPane({ builtWith, back }: { builtWith: BuiltWith | null; back?: () => void }) {
  return (
    <Pane title="Built with" subtitle="What Colonizer is built with — and where that is written down" back={back}>
      {!builtWith ? (
        <p className="flex items-center gap-2 text-body-sm text-muted">
          <Spinner /> Loading…
        </p>
      ) : builtWith.uses.length === 0 ? (
        <p className="text-body-sm text-muted">Nothing published for this venture yet.</p>
      ) : (
        <div className="space-y-4">
          <div className="divide-y divide-border">
            {builtWith.uses.map((use) => (
              <div key={use.id} className="flex flex-wrap items-baseline gap-x-4 gap-y-1 py-3">
                <span className="w-40 shrink-0 text-body-sm text-muted">{use.phrase}</span>
                <div className="min-w-0 flex-1">
                  <span className="flex flex-wrap items-center gap-2">
                    <a className="inline-flex items-center gap-1 text-body-sm font-medium text-accent hover:underline" href={use.url} target="_blank" rel="noreferrer">
                      {use.name} <IconExternal size={12} />
                    </a>
                    {/* The registry's own word, not a dot: `live` and `planned` read the same to a person and a screen reader. */}
                    <Badge tone={STATUS_TONE[use.status]}>{use.status}</Badge>
                  </span>
                  <p className="mt-0.5 text-small-lg text-muted">{use.note}</p>
                </div>
              </div>
            ))}
          </div>
          <ProvenanceLinks builtWith={builtWith} />
        </div>
      )}
    </Pane>
  );
}
