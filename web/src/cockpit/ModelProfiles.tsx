// Saved model profiles in the model menu: a named set of role → model choices, kept in the install
// config (GET/POST/PUT/DELETE /api/models/profiles) so every device sees the same list. A profile
// loads into the menu's draft for the chosen scope; the ordinary Apply (new colonies only, or also the
// running ones) then switches to it, so a profile is applied exactly as a hand-made switch is. The
// Profiles list in the menu saves, renames and deletes them (starters are derived, never stored).
import type { ModelProfile } from "../types";

/** "orchestrator opus · subagents deepseek-flash", for an option's title. */
export function profileSummary(profile: ModelProfile): string {
  const words: Record<string, string> = {
    model: "orchestrator",
    subagent_model: "subagents",
    background_model: "background",
    summary_model: "summary",
    small_model: "small",
    model_low: "small tasks",
    model_high: "large tasks",
  };
  return Object.entries(profile.roles)
    .map(([role, model]) => `${words[role] ?? role} ${model ? model.slice(model.indexOf("/") + 1) : "default"}`)
    .join(" · ");
}
