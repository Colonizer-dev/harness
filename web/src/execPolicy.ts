// The exec policy's save-time check (issue #924), mirrored from the mothership's
// crates/colonizer/src/exec_policy.rs for instant feedback in the org settings form. A policy is
// accepted only when the runner's parsePolicy (modules/agents/*/execpolicy.mjs) would keep it whole:
// a rule it dropped would be one the operator believes holds. The server checks again on save, and
// the shared fixture modules/agents/claude-code/test/fixtures/execpolicy-valid.json drives all three.

const MAX_LENGTH = 64 * 1024;
const REGEX_MAX_CHARS = 500;
const DECISIONS = ["deny", "ask", "allow"];

type Json = unknown;
const isObject = (v: Json): v is Record<string, Json> => typeof v === "object" && v !== null && !Array.isArray(v);

/** What is wrong with a `command`/`script` value, or null: one pattern or a non-empty list, not all empty. */
function patternsProblem(value: Json): string | null {
  const list = Array.isArray(value) ? value : [value];
  if (!list.length || list.some((p) => typeof p !== "string")) return "must be a pattern or a list of patterns";
  if ((list as string[]).some((p) => p.length > REGEX_MAX_CHARS)) return `patterns are at most ${REGEX_MAX_CHARS} characters`;
  if ((list as string[]).every((p) => p === "")) return 'is empty; a deliberate catch-all is "."';
  return null;
}

function ruleProblem(rule: Json): string | null {
  if (!isObject(rule)) return "a rule must be an object";
  if (typeof rule.decision !== "string" || !DECISIONS.includes(rule.decision.toLowerCase()))
    return '"decision" must be "deny", "ask" or "allow"';
  let predicates = 0;
  for (const key of ["command", "script"]) {
    if (!(key in rule)) continue;
    const problem = patternsProblem(rule[key]);
    if (problem) return `"${key}" ${problem}`;
    predicates++;
  }
  if ("touches" in rule) {
    const touches = rule.touches;
    if (!Array.isArray(touches) || !touches.every((g) => typeof g === "string" && g.trim() !== ""))
      return '"touches" must be a list of path globs';
    if (!touches.some((g) => !(g as string).startsWith("!")))
      return '"touches" needs at least one path glob that is not a "!" exclusion';
    predicates++;
  }
  if ("writes_outside" in rule && rule.writes_outside !== false) {
    if (rule.writes_outside !== true && rule.writes_outside !== "strict") return '"writes_outside" must be true or "strict"';
    predicates++;
  }
  if ("writes_git" in rule && rule.writes_git !== false) {
    if (rule.writes_git !== true) return '"writes_git" must be true';
    predicates++;
  }
  if (!predicates)
    return 'a rule needs "command", "script", "touches", "writes_outside" or "writes_git", or it would match every command';
  return null;
}

/** The first problem with an exec policy's JSON text, in the server's words, or null when it saves. Blank is no policy. */
export function execPolicyProblem(text: string): string | null {
  if (text.trim() === "") return null;
  if (text.length > MAX_LENGTH) return "the exec policy is larger than 64 KiB";
  let policy: Json;
  try {
    policy = JSON.parse(text);
  } catch (e) {
    return `the exec policy is not valid JSON: ${e instanceof Error ? e.message : String(e)}`;
  }
  if (!isObject(policy) || !Array.isArray(policy.rules)) return 'the exec policy must be an object with a "rules" array';
  for (const [i, rule] of policy.rules.entries()) {
    const problem = ruleProblem(rule);
    if (!problem) continue;
    const id = isObject(rule) && typeof rule.id === "string" && rule.id.trim() ? JSON.stringify(rule.id.trim()) : String(i + 1);
    return `exec policy rule ${id}: ${problem}`;
  }
  return null;
}
