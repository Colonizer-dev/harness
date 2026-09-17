// Subagents shown as settlers: a colony's orchestrator sends settlers out to do the work, and each one's
// role says what kind of work it is. Claude Code's agent types stay the source of truth; this only names them.

/** Roles for the agent types Claude Code ships, and the ones colonies commonly start. */
const ROLES: Record<string, string> = {
  explore: "Scout",
  "general-purpose": "Builder",
  plan: "Surveyor",
  claude: "Pioneer",
  "statusline-setup": "Signwright",
  "code-reviewer": "Inspector",
  reviewer: "Inspector",
  "security-reviewer": "Warden",
  "test-runner": "Tester",
  tester: "Tester",
  architect: "Cartographer",
  debugger: "Tracker",
  "docs-writer": "Scribe",
  "doc-updater": "Scribe",
  "build-error-resolver": "Mender",
  "refactor-cleaner": "Mason",
};

function titleCase(name: string): string {
  return name
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((word) => word[0].toUpperCase() + word.slice(1))
    .join(" ");
}

/**
 * The settler name for a subagent type: "Scout Settler" for Explore, "Builder Settler" for general-purpose or no type, and
 * the type itself in title case for anything unknown ("Api Designer Settler"). `ordinal` numbers the second and later
 * settlers of the same role in one colony.
 */
export function settlerName(agentType: string | null | undefined, ordinal = 1): string {
  const type = (agentType ?? "").trim();
  // No type means Claude Code's default, general-purpose.
  const role = ROLES[type.toLowerCase()] ?? (type ? titleCase(type) : "Builder");
  return ordinal > 1 ? `${role} Settler ${ordinal}` : `${role} Settler`;
}
