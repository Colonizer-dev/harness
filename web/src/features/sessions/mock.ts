// The `sessions` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { ago, clone, isLive, now, sleep } from "../../mockShared";
import { canPublish } from "../../components/ui";
import { ISSUES } from "../../features/repos/mock";
import { isTerminal } from "../../notifications";
import { MockSession, baseSession, mockTerminal } from "./mockSession";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { SessionsApi } from "./api";

export function sessionsMock(ms: MockState): SessionsApi {
  return {
    burnDown: () => ms.later(() => ms.burnDown),
    stopBurnDown: async () => {
      await sleep(200);
      // The real endpoint stops every live/queued burn_down colony too; the payload's colony
      // counts reflect that so the Overview card empties the live/queued columns on stop.
      ms.burnDown = { ...ms.burnDown, enabled: false, state: "disabled", colonies: { ...ms.burnDown.colonies, live: 0, queued: 0 } };
    },
    findings: (id) => ms.later(() => (id === "demo1234" ? ms.FINDINGS : [])),
    sessionCommits: () => ms.later(() => ({ commits: [] })),
    sessionDiff: (id) =>
      ms.later(() => {
        const s = ms.sessions.get(id)?.session;
        // No pull request, no card to fill: an empty answer, where the real route would 409 with no worktree.
        if (!s?.pr_url) return { id, repo: s?.repo ?? "", base: s?.base ?? null, files: [], added: 0, removed: 0, diff: "", truncated: false };
        const files = [
          { path: "web/src/cockpit/Inspector.tsx", added: 41, removed: 6 },
          { path: "web/src/api.ts", added: 8, removed: 0 },
          { path: "web/src/types.ts", added: 12, removed: 1 },
          { path: "web/src/cockpit/Inspector.test.tsx", added: 55, removed: 2 },
          { path: "docs/gaps.md", added: 1, removed: 1 },
          { path: "changelog.d/611.added.md", added: 4, removed: 0 },
        ];
        return {
          id,
          repo: s.repo,
          base: s.base,
          files,
          added: files.reduce((n, f) => n + f.added, 0),
          removed: files.reduce((n, f) => n + f.removed, 0),
          diff: "",
          truncated: false,
        };
      }),
    sessions: () =>
      ms.later(() => [...ms.sessions.values()].map((s) => s.session).sort((a, b) => b.updated_at.localeCompare(a.updated_at))),
    session: async (id) =>
      ms.later(() => {
    const s = clone(ms.find(id).session);
    // The single-session route alone carries the stuck-colony readout (issue #230).
    if (id === "demo1234") {
      const quota = "API Error: quota has been exhausted. The quota will reset at 09-23 07:54:00 UTC.";
      s.diagnosis ??= { state: "waiting_on_provider", text: `waiting on provider: quota exhausted, resets 09-23 07:54:00 UTC`, resets_at: "2026-09-23T07:54:00Z" };
      s.recent_events ??= [
        { seq: 41, ts: ago(65), type: "status", summary: "working" },
        { seq: 42, ts: ago(22), type: "assistant_text", summary: quota },
      ];
    }
    return s;
      }),
    createSession: async (body) => {
      await sleep(450);
      const id = Math.random().toString(16).slice(2, 10);
      const issueNumber = body.issue ?? null;
      const issue = ISSUES[body.repo]?.find((i) => i.number === issueNumber);
      const title = issueNumber == null ? "Open colony" : (body.title ?? issue?.title ?? `Issue #${issueNumber}`);
      // `after` stacks the new colony on another's branch: `parent` records it and `base` moves to
      // the parent's branch, which is what the mothership does. Creating a stack is API-only.
      const after = body.after ? ms.sessions.get(body.after)?.session ?? null : null;
      const session = new MockSession(
    {
      ...baseSession(id, body.repo, issueNumber, title),
      autopilot: body.autopilot ?? true,
      // A launch choice carries through to the session record, as on the mothership; omitted
      // means the session fell back to the publish module's setting and reports none of its own.
      autofix: body.autofix,
      automerge: body.automerge,
      supply_chain: body.supply_chain ?? null,
      parent: after?.id ?? null,
      base: after?.branch ?? "main",
    },
    false,
    body.instructions ?? null,
      );
      ms.sessions.set(id, session);
      ms.colonyActivity(body.origin === "chat" ? "chat.colony" : body.origin === "colonize" ? "colonize.colony" : "colony.launch", session.session);
      return clone(session.session);
    },
    resumeSession: async (id) => {
      await sleep(250);
      const s = ms.find(id);
      if (s.session.status !== "stopped" && s.session.status !== "failed") {
    throw new ApiError("this colony can't be resumed", 409);
      }
      s.patch({ status: "starting", error: null });
      ms.colonyActivity("colony.resume", s.session);
      return clone(s.session);
    },
    prewarmSession: async () => {},
    publishSession: async (id) => {
      const s = ms.find(id);
      if (!canPublish(s.session)) throw new ApiError("this colony cannot be published", 409);
      const live = isLive(s.session.status);
      s.halt();
      s.patch({ status: "publishing", error: null });
      s.log(live ? "Stopping the agent and removing the microVM" : "Finishing the last publish on the existing worktree");
      setTimeout(() => {
    s.log(`Committed 3 files on ${s.session.branch} and pushed`);
    s.patch({
      status: "pr_opened",
      mesh: null,
      pr_url: `https://github.com/${s.session.repo}/pull/${60 + Math.floor(Math.random() * 40)}`,
      publish_stage: "pr_opened",
    });
    s.log("Opened pull request");
      }, 1800);
      return clone(s.session);
    },
    stopSession: async (id) => {
      const s = ms.find(id);
      // Like the server: a colony already over answers `already_stopped` untouched, a queued one
      // leaves the queue, and only a publishing one is refused.
      if (isTerminal(s.session.status)) return { ...clone(s.session), result: "already_stopped" };
      if (s.session.status === "queued") {
    s.patch({ status: "stopped" });
    s.log("Left the queue before it started");
    return { ...clone(s.session), result: "stopped" };
      }
      if (!isLive(s.session.status)) throw new ApiError("session is not running", 409);
      s.halt();
      s.patch({ status: "stopped", mesh: null });
      ms.logActivity({ kind: "outcome.stopped", actor: "you", via: "cockpit", org: s.session.org, repo: s.session.repo, issue: s.session.issue, colony: s.session.id, title: s.session.issue_title });
      s.log("microVM stopped and removed; the worktree was kept");
      return { ...clone(s.session), result: "stopped" };
    },
    keepSession: async (id) => {
      await sleep(200);
      const s = ms.find(id);
      const superseded = s.session.superseded;
      // Like the server: 409 for a colony that is not superseded (or was kept already).
      if (!superseded || superseded.kept) throw new ApiError("this colony is not superseded; there is nothing to keep", 409);
      s.patch({ superseded: { ...superseded, kept: true } });
      s.log("kept: this colony will start even though a merged pull request covered its work");
      return clone(s.session);
    },
    seenSession: async (id) => {
      await sleep(120);
      ms.find(id).patch({ unseen_failure: false });
    },
    deleteSession: async (id, opts) => {
      const s = ms.find(id);
      if (isLive(s.session.status) || s.session.status === "publishing") throw new ApiError("stop the colony first", 409);
      s.halt();
      ms.sessions.delete(id);
      ms.colonyActivity("colony.delete", s.session);
      // The logs are archived first (issue #496); `purge_logs` also takes that archived bundle.
      const prior = ms.archiveEntries.filter((e) => e.session === id);
      for (let i = ms.archiveEntries.length - 1; i >= 0; i--) if (ms.archiveEntries[i].session === id) ms.archiveEntries.splice(i, 1);
      if (opts?.purgeLogs) return { deleted: id, leftover: null, archived: null, purged_bundles: prior.length, purge_error: null };
      const revision = (prior.at(-1)?.revision ?? 0) + 1;
      ms.archiveEntries.push({ session: id, repo: s.session.repo, issue: s.session.issue, title: s.session.issue_title, status: s.session.status, bundle: `${id}-rev${revision}.tar.zst`, bytes: 4_194_304, archived_at: now(), revision });
      return { deleted: id, leftover: null, archived: `${id}-rev${revision}.tar.zst`, purged_bundles: 0, purge_error: null };
    },
    cleanupSession: async (id) => {
      const s = ms.find(id);
      if (isLive(s.session.status) || s.session.status === "publishing") throw new ApiError("stop the colony first", 409);
      s.patch({ cleaned_up: true });
      s.log("Removed the worktree and local branch");
      return clone(s.session);
    },
    setKeep: async (id, keep) => {
      const s = ms.find(id);
      s.patch({ keep_worktree: keep });
      s.log(keep ? "Worktree kept: automatic reclamation will skip this colony" : "Worktree released back to automatic reclamation");
      return clone(s.session);
    },
    behindSession: async (id) => {
      const s = ms.find(id).session;
      return ms.later(() => ({ behind_by: s.base ? 0 : null, base: s.base, branch: s.branch }));
    },
    catchUpSession: async (id) => {
      const s = ms.find(id);
      return ms.later(() => ({ session: clone(s.session), merged: false, conflicts: [], behind_by: s.session.base ? 0 : null }));
    },
    openTerminal: (id) => mockTerminal(ms.sessions.get(id))
  };
}
