// The colony page's "Living Brain" link (crates/colonizer/src/living_brain): shown once the colony's
// session reached the brain, pointing at its note there. Quiet otherwise — a colony that was never
// sent, a mothership that is not connected, or one too old to have the route all render nothing.
import { useEffect, useState } from "react";
import { useApi } from "../../context";
import { IconExternal } from "../../components/icons";
import { buttonClass } from "../../components/ui";
import type { LivingBrainColony } from "./types";

export function LivingBrainLink({ id, status }: { id: string; status: string }) {
  const api = useApi();
  const [colony, setColony] = useState<LivingBrainColony | null>(null);
  useEffect(() => {
    let live = true;
    // `status` is a dependency so the link appears once a finished turn has been sent.
    api
      .livingBrainColony?.(id)
      .then((c) => live && setColony(c))
      .catch(() => live && setColony(null));
    return () => {
      live = false;
    };
  }, [api, id, status]);
  return <LivingBrainAnchor colony={colony} />;
}

export function LivingBrainAnchor({ colony }: { colony: LivingBrainColony | null }) {
  if (!colony?.note_url) return null;
  return (
    <a href={colony.note_url} target="_blank" rel="noopener noreferrer" className={buttonClass("secondary")} title="This colony's session in Living Brain">
      Living Brain <IconExternal size={12} />
    </a>
  );
}
