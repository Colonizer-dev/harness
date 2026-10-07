// Stacks: a colony may branch from another colony's branch instead of the default one, so its
// review cannot land until the colony below it does. Everything here is a pure reading of the
// colony list, in the style of sessionOrder: no fetching, no state, so a bad `parent` link can
// never hang a render — a cycle, a link that leaves the list, or an absurd depth all end the walk.
import type { Session } from "./types";

/**
 * How far a chain of ancestors is followed before giving up. Real stacks are two or three deep;
 * the cap exists so a corrupt or hostile `parent` loop costs a short chain, not a frozen tab.
 */
export const MAX_STACK_DEPTH = 32;

/** The colony this one is stacked on, or null when it branches from the default branch — or when the parent has aged out of the list, which must read as unstacked rather than crash. */
export function parentOf(sessions: Session[], session: Session): Session | null {
  if (!session.parent) return null;
  return sessions.find((s) => s.id === session.parent) ?? null;
}

/** The chain of ancestors, root first: the colony everything ultimately branches from, then down to this one's direct parent. Cycles, dangling links and the depth cap all just end the walk. */
export function ancestorsOf(sessions: Session[], session: Session): Session[] {
  const chain: Session[] = [];
  const seen = new Set<string>([session.id]);
  let current = session;
  while (chain.length < MAX_STACK_DEPTH) {
    const parent = parentOf(sessions, current);
    if (!parent || seen.has(parent.id)) break;
    seen.add(parent.id);
    chain.push(parent);
    current = parent;
  }
  return chain.reverse();
}

/** The colonies stacked directly on this one, in list order. A self-parenting colony does not count itself. */
export function childrenOf(sessions: Session[], session: Session): Session[] {
  return sessions.filter((s) => s.parent === session.id && s.id !== session.id);
}
