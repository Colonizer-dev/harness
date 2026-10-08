import type { Answers, ModelTokens, Question, Session } from "../sessions/types";
import type { MemoryProposal, MemoryScope } from "../memory/types";

export type AgentState = "idle" | "working" | "waiting_for_answer" | "error" | "exited";

export type LogLevel = "info" | "warn" | "error";

/** The subagent that produced an event. Absent on the orchestrator's own events. */
export interface AgentRef {
  /** The Task tool call that started it, which is also its identity for the run. */
  id: string;
  /** The subagent type, or its task description when the type was not named. */
  name: string;
  description?: string | null;
}

/**
 * Who set a recorded line in motion (issue #312): an envelope field beside `seq`/`ts`/`agent` on every
 * line of `events.jsonl` and `harness.jsonl`, and on each stream event. Absent on lines recorded before
 * the field existed, which readers infer as before (a `watchdog-` message id, an answered question).
 * Exception: a `memory_proposal`'s `origin` names the proposer (§6.2), not the envelope — those lines
 * are never stamped (docs/agent-events.schema.json `#/$defs/origin`).
 */
export const ORIGINS = [
  "user",
  "agent",
  "subagent",
  "watchdog",
  "autonomy",
  "burn_down",
  "redteam",
  "notify",
  "system",
] as const;
export type Origin = (typeof ORIGINS)[number];

/** The closed vocabulary of `boundary.kind` (docs/agent-events.schema.json, issue #609). */
export type BoundaryKind =
  | "exec_policy_deny"
  | "exec_policy_ask_bypass_attempt"
  | "path_policy_denied"
  | "path_policy_unbound"
  | "egress_denied"
  | "publish_rewrite_refused"
  | "sandbox_denied";

/**
 * A control refused something (issue #609): the typed record the watchdog's control-defeat signature
 * reads. Reporting only — the control decided before it was written. The runner, agentd or the
 * mothership itself appends it; the chat shows each one as a muted row where it happened.
 */
export interface BoundaryRecord {
  type: "boundary";
  kind: BoundaryKind;
  /** The control or rule that decided: `exec_policy:<rule>`, `egress`, `path_policy:masked`, `gitfile`, … */
  control: string;
  /** What was refused, one redacted line. */
  detail: string;
  /** The host or path refused, when the reporter could name one. */
  target?: string;
  /** When the control decided, on the reporter's clock. */
  at: string;
}

interface Sequenced {
  seq?: number;
  ts?: string;
  agent?: AgentRef;
  origin?: Origin;
}

export type AgentEventBody =
  | { type: "status"; state: AgentState; detail?: string | null }
  | { type: "user_message"; id: string; text: string }
  | { type: "assistant_text_delta"; message_id: string; block_index: number; delta: string }
  | { type: "assistant_text"; message_id: string; block_index: number; text: string }
  | { type: "thinking"; message_id: string; block_index: number; text: string }
  | { type: "tool_call"; message_id: string; tool_call_id: string; name: string; input: Record<string, unknown> }
  | { type: "tool_result"; tool_call_id: string; output: string; is_error: boolean }
  /**
   * `risk` is the question's risk class (§2 rules), which routes the autonomy judge's ceiling (§6.2b);
   * absent means workspace_write, and any value reads as above every ceiling until judged.
   */
  | {
      type: "question";
      question_id: string;
      message_id?: string;
      risk?: "read_only" | "workspace_write" | "publish_affecting" | "credential_adjacent";
      questions: Question[];
    }
  | { type: "question_answered"; question_id: string; answers: Answers; response?: string | null }
  | {
      type: "turn_end";
      is_error: boolean;
      result: string | null;
      cost_usd: number | null;
      duration_ms: number | null;
      /** Colony-cumulative totals as of this turn, not this turn's own usage (docs/protocol.md §4). */
      model_usage?: Record<string, ModelTokens>;
    }
  | { type: "log"; level: LogLevel; message: string }
  /** The model the colony's next turns use: sent at start with no `previous`, then after each `set_model` that took. */
  | { type: "model_changed"; model: string; previous: string | null }
  /**
   * A proposed shared-memory note (docs/protocol.md §6.2). Absent or null scope means repo; absent
   * tags mean none. The wire's `origin` on this one event names the proposer — "orchestrator",
   * "subagent:<name>", "background:<name>" (§3 Origins) — not the envelope's, so it is not declared
   * here: it would collide with `Sequenced`'s envelope `origin`, and the stream reads only the
   * watermark off this body.
   */
  | { type: "memory_proposal"; scope?: MemoryScope | null; title: string; content: string; tags?: string[] }
  /** A confirmed problem outside the task (§6.6), which the mothership files as a GitHub issue. */
  | { type: "finding"; title: string; body: string; evidence: string }
  /**
   * Jev compaction's per-chunk decisions for one pass (#475), shadow telemetry the harness grades
   * into its data-dir-wide `jev_ladder.jsonl`; the web types it but renders nothing.
   */
  | {
      type: "jev_ladder";
      applied: boolean;
      pre_tokens?: number;
      post_tokens?: number;
      trigger?: string;
      decisions: Array<{
        tool_call_id: string;
        tool: string;
        action: "keep" | "drop_result" | "drop_call";
        keep_call?: number;
        keep_result?: number;
      }>;
    }
  /**
   * The agent reached for a path the path policy masks or write-protects (docs/path-policy.md,
   * #647). Reporting only — the mount enforced before this ran. The harness turns it into a colony
   * log line and a History entry per distinct (access, path); the stream types it and renders
   * nothing of its own.
   */
  | { type: "path_policy"; access: "read" | "write"; policy: "masked" | "protected"; path: string; tool?: string }
  /** A control refused something (issue #609); a muted row in the chat timeline. */
  | BoundaryRecord
  /**
   * The mothership's independent verdict on a completion claim (§6.3, Autopilot): tests re-run in a
   * fresh checkout and the git state read directly, never the agent's own account. Host-generated,
   * like the finding-chain events, so the runner-event schema does not list it.
   */
  | {
      type: "verification";
      /** `inconclusive` (issue #672): a check failed on the colony's work but fails on the merge-base too, so it is not this colony's doing — autopilot publishes anyway. */
      verdict: "confirmed" | "contradicted" | "inconclusive" | "unverifiable";
      by_declaration: boolean;
      summary: string;
      contradictions: string[];
      /** Observations that do not change the verdict, e.g. a described path missing beside ones that are there. Absent on events recorded before it existed. */
      advisories?: string[];
      /** Checks that failed on the merge-base as well, one reviewer-ready clause each (issue #672). Absent on events recorded before it existed. */
      inconclusive?: string[];
      command: string | null;
      command_source:
        | "config"
        | "packageManager"
        | "bun.lock"
        | "bun.lockb"
        | "pnpm-lock.yaml"
        | "yarn.lock"
        | "package-lock.json"
        | "npm-shrinkwrap.json"
        | "package.json"
        | "Cargo.toml"
        | "Makefile"
        | null;
      exit_code: number | null;
      tests_ms: number | null;
      commits: number;
      files_changed: string[];
      snapshot: string | null;
      ms: number;
    };

export type AgentEvent = Sequenced & AgentEventBody;

/**
 * What the mothership derived from the colony's whole run (issue #1210), so the cockpit need not replay it:
 * the settlers in order of first appearance, the latest cost, the turn count, the brief and the last state.
 */
export interface HistorySummary {
  last_seq: number;
  events: number;
  turns: number;
  cost_usd: number | null;
  /** The colony's first message (`user_message` with id `initial`) as it was recorded. */
  brief: (Sequenced & { type: "user_message"; id: string; text: string }) | null;
  model: string | null;
  agent_state: { state: AgentState; detail: string | null } | null;
  settlers: HistorySettler[];
}

export interface HistorySettler {
  id: string;
  name: string;
  description: string | null;
  steps: number;
  errors: number;
  last_tool: string | null;
}

/** What names the page behind a loaded one: the run, the `seq` and the byte the oldest loaded event sits at. */
export interface HistoryCursor {
  epoch: number;
  seq: number;
  offset: number;
}

/** `GET /api/sessions/{id}/events?before=…`: one page of the log, oldest event first. */
export interface EventsPage {
  events: AgentEvent[];
  has_more: boolean;
  oldest_seq: number;
  epoch: number;
  offset: number;
  /** The colony-cumulative `model_usage` as of the last turn end before the page. */
  baseline_usage: Record<string, ModelTokens> | null;
  run_epoch: number;
}

export type ServerFrame =
  | AgentEvent
  | { type: "session"; session: Session }
  | { type: "harness_log"; level: LogLevel; message: string; ts: string; origin?: Origin }
  | { type: "memory_proposed"; proposal: MemoryProposal }
  | { type: "run_epoch"; epoch: number }
  /**
   * First paint with a `limit` (issue #1210): sent before the newest page's events. `has_more` says older
   * events are on record, behind the cursor (`epoch`, `oldest_seq`, `offset`).
   */
  | {
      type: "history";
      has_more: boolean;
      oldest_seq: number;
      epoch: number;
      offset: number;
      baseline_usage: Record<string, ModelTokens> | null;
      summary: HistorySummary;
    }
  /** The backlog replay is complete; everything after it is live. */
  | { type: "replay_done"; seq: number };

export type ClientCommand =
  | { type: "user_message"; text: string }
  | { type: "answer"; question_id: string; answers: Answers; response: string | null }
  | { type: "interrupt" }
  /** Switches the model for the colony's next turns, keeping the conversation; `model_changed` confirms it. */
  | { type: "set_model"; model: string };
