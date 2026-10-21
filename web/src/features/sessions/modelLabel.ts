// Format the lead model and tier for display (issue #1366).

import type { Session } from "./types";

/**
 * Format the lead model and routing tier for a session.
 * Returns a label like "claude-opus-5-5 · high", "module model · medium" when model is missing but tier is known,
 * the model alone if no tier, or null when neither is present.
 */
export function leadModelLabel(session: Session): string | null {
  // Prefer model_routing over model_tier for tier, with model_routing.model as the primary source
  const model = session.model_routing?.model ?? null;
  const tier = session.model_routing?.tier ?? session.model_tier ?? null;

  // No model and no tier
  if (!model && !tier) {
    return null;
  }

  // Has both model and tier
  if (model && tier) {
    return `${model} · ${tier}`;
  }

  // Has model but no tier
  if (model) {
    return model;
  }

  // Has tier but no model — show "module model · tier"
  if (tier) {
    return `module model · ${tier}`;
  }

  return null;
}
