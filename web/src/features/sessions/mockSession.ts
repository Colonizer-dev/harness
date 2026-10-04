// MockSession (the mock colony's event stream) with its session fixtures, split out of
// src/mock.ts (issue #827). Used by src/mockState.ts and the sessions mock.
import { MockSocket, ago, clone, isLive, now, rhythm, sleep } from "../../mockShared";
import type { AgentEvent, AgentEventBody, AgentRef, Answers, LogLevel, MemoryProposal, ModelTokens, Question, Session, SessionStatus } from "../../types";
import type { SocketLike } from "../../http";

export const DEMO_QUESTIONS: Question[] = [
  {
    question: "How should guest checkout create the order?",
    header: "Guest flow",
    multi_select: false,
    options: [
      {
        label: "Guest cart by email",
        description: "Keep guests anonymous and attach the order to the email address they enter.",
      },
      {
        label: "Silent account",
        description: "Create a passwordless account behind the scenes so the order appears if they sign up later.",
      },
      {
        label: "Require sign-in",
        description: "Treat the error as intended and show a clear sign-in prompt instead.",
      },
    ],
  },
  {
    question: "What else should go into this pull request?",
    header: "Scope",
    multi_select: true,
    options: [
      {
        label: "Regression test",
        description: "Add an API test that checks out as a guest.",
        preview:
          '```ts\nit("lets guests check out", async () => {\n  const res = await api.post("/checkout", {\n    email: "guest@example.com",\n    items: [{ sku: "TSHIRT-M", qty: 1 }],\n  });\n  expect(res.status).toBe(201);\n});\n```',
      },
      { label: "Update docs", description: "Document the guest checkout flow in docs/checkout.md." },
      { label: "Error telemetry", description: "Log checkout failures with a reason so regressions surface sooner." },
    ],
  },
];

export const GUEST_TEST = `import { checkout } from "../src/checkout/api";

test("lets guests check out", async () => {
  const order = await checkout({ email: "guest@example.com", items: [{ sku: "mug", qty: 1 }] });
  expect(order.user).toBeNull();
});
`;

export const SESSION_TS = `import { createGuestCart } from "./cart";

export async function createSession(req: Request) {
  const user = await currentUser(req);
  if (!user) throw new Error("guest checkout disabled");
  return { user, cart: await loadCart(user.id) };
}`;

export class MockSession {
  readonly session: Session;
  private readonly events: AgentEvent[] = [];
  /** Unsequenced frames (memory_proposed) replayed on every attach; the UI dedupes them. */
  private readonly frames: { afterSeq: number; frame: object }[] = [];
  private readonly logs: { type: "harness_log"; level: LogLevel; message: string; ts: string }[] = [];
  private readonly sockets = new Set<MockSocket>();
  private seq = 0;
  private started = false;
  private generation = 0;
  private cost = 0;
  /** Colony-cumulative per-model usage, as the runner reports it in every `turn_end` (docs/protocol.md §4). */
  private modelUsage: Record<string, ModelTokens> = {};
  /** The model the next turn uses; `set_model` switches it and `model_changed` reports it. */
  private model = "claude-opus-5";
  private pendingQuestion: string | null = null;
  private userMessages = 0;

  private readonly instructions: string | null;

  constructor(session: Session, history = false, instructions: string | null = null) {
    this.session = session;
    this.instructions = instructions;
    if (history) this.seedHistory();
  }

  private broadcast(frame: object): void {
    const text = JSON.stringify(frame);
    for (const socket of this.sockets) socket.deliver(text);
  }

  /** `agent` rides along for a subagent's events, as the runner sends it. */
  emit(body: AgentEventBody & { agent?: AgentRef }, ts = now()): void {
    const event = { ...body, seq: ++this.seq, ts } as AgentEvent;
    this.events.push(event);
    this.session.last_activity_at = ts;
    this.broadcast(event);
    // Any agent event clears the watchdog flag (§6.3).
    if (this.session.attention) this.patch({ attention: null });
    if (body.type === "status" && isLive(this.session.status)) {
      const map: Partial<Record<string, SessionStatus>> = {
        working: "running",
        waiting_for_answer: "waiting_for_answer",
        idle: "idle",
      };
      const status = map[body.state];
      if (status && status !== this.session.status) this.patch({ status });
    }
    if (body.type === "turn_end" && body.cost_usd != null) this.patch({ cost_usd: body.cost_usd });
  }

  /**
   * Adds one turn's per-model usage to the colony's running total and returns it, as the runner's cumulative
   * `model_usage` does; the snapshot is cloned so replayed earlier turns keep the totals they had then.
   */
  private turnUsage(perTurn: Record<string, ModelTokens>): Record<string, ModelTokens> {
    for (const [model, tokens] of Object.entries(perTurn)) {
      const total = (this.modelUsage[model] ??= { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, thinking_tokens: 0 });
      total.input_tokens += tokens.input_tokens;
      total.output_tokens += tokens.output_tokens;
      total.cache_read_tokens += tokens.cache_read_tokens;
      total.cache_write_tokens += tokens.cache_write_tokens;
      total.thinking_tokens += tokens.thinking_tokens;
    }
    return clone(this.modelUsage);
  }

  log(message: string, level: LogLevel = "info"): void {
    const entry = { type: "harness_log" as const, level, message, ts: now() };
    this.logs.push(entry);
    this.broadcast(entry);
  }

  patch(changes: Partial<Session>): void {
    Object.assign(this.session, changes, { updated_at: now() });
    this.broadcast({ type: "session", session: this.session });
  }

  attach(socket: MockSocket, since: number): void {
    this.sockets.add(socket);
    socket.deliver(JSON.stringify({ type: "session", session: this.session }));
    for (const entry of this.logs.slice(-200)) socket.deliver(JSON.stringify(entry));
    // Frames go out where they happened, so a replayed notice lands after the same message.
    let next = 0;
    const flushFrames = (upTo: number) => {
      while (next < this.frames.length && this.frames[next].afterSeq < upTo) socket.deliver(JSON.stringify(this.frames[next++].frame));
    };
    for (const event of this.events) {
      flushFrames(event.seq ?? 0);
      if ((event.seq ?? 0) > since) socket.deliver(JSON.stringify(event));
    }
    flushFrames(Infinity);
    if (!this.started && isLive(this.session.status)) {
      this.started = true;
      void this.intro();
    }
  }

  detach(socket: MockSocket): void {
    this.sockets.delete(socket);
  }

  command(data: unknown): void {
    if (typeof data !== "string") return;
    let command: { type?: string; text?: string; question_id?: string; answers?: Answers; response?: string | null; model?: string };
    try {
      command = JSON.parse(data);
    } catch {
      return;
    }
    if (command.type === "answer" && command.question_id && command.question_id === this.pendingQuestion) {
      void this.onAnswer(command.question_id, command.answers ?? {}, command.response ?? null);
    } else if (command.type === "user_message" && command.text) {
      void this.onUserMessage(command.text);
    } else if (command.type === "interrupt") {
      this.generation += 1;
      this.emit({ type: "log", level: "info", message: "Interrupted by user" });
      this.emit({ type: "status", state: this.pendingQuestion ? "waiting_for_answer" : "idle" });
    } else if (command.type === "set_model" && command.model) {
      void this.onSetModel(command.model);
    }
  }

  /** The runner confirms a switch a moment later; the next turn uses the new model. */
  private async onSetModel(model: string): Promise<void> {
    await sleep(300);
    const previous = this.model;
    this.model = model;
    this.emit({ type: "model_changed", model, previous });
  }

  halt(): void {
    this.generation += 1;
    this.pendingQuestion = null;
  }

  private alive(generation: number): boolean {
    return generation === this.generation && isLive(this.session.status);
  }

  /** `agent` marks what a subagent says, as the runner does (docs/protocol.md §2). */
  private async streamText(generation: number, messageId: string, text: string, agent?: AgentRef): Promise<boolean> {
    const words = text.match(/\S+\s*/g) ?? [text];
    const by = agent ? { agent } : {};
    // Real deltas arrive the way colonies' event logs show them: several at once (150–250 characters in the same
    // millisecond), then nothing for 100–570 ms.
    const next = rhythm(messageId.length * 7919 + text.length);
    for (let i = 0; i < words.length; ) {
      const burst = 4 + Math.floor(next() * 8);
      for (let k = 0; k < burst && i < words.length; k++) {
        if (!this.alive(generation)) return false;
        const count = 2 + Math.floor(next() * 3);
        this.emit({ type: "assistant_text_delta", message_id: messageId, block_index: 0, delta: words.slice(i, i + count).join(""), ...by });
        i += count;
      }
      await sleep(100 + next() * 470);
    }
    if (!this.alive(generation)) return false;
    this.emit({ type: "assistant_text", message_id: messageId, block_index: 0, text, ...by });
    return true;
  }

  private async tool(
    generation: number,
    messageId: string,
    id: string,
    name: string,
    input: Record<string, unknown>,
    output: string,
    delay = 800,
    agent?: AgentRef,
    isError = false,
  ): Promise<boolean> {
    if (!this.alive(generation)) return false;
    const by = agent ? { agent } : {};
    this.emit({ type: "tool_call", message_id: messageId, tool_call_id: id, name, input, ...by });
    await sleep(delay);
    if (!this.alive(generation)) return false;
    this.emit({ type: "tool_result", tool_call_id: id, output, is_error: isError, ...by });
    return true;
  }

  /** A stopped session whose only turn failed, like a run with a bad Claude token. */
  seedFailedHistory(): void {
    this.started = true;
    this.log("Worktree created, microVM booted, joined mesh");
    this.emit({
      type: "user_message",
      id: "initial",
      text: `You are resolving GitHub issue #${this.session.issue} in the repository ${this.session.repo}.\n\n<issue>\nTitle: ${this.session.issue_title}\n</issue>\n\nHow to work:\n1. Read the relevant code first.\n2. Ask before product decisions.`,
    });
    this.emit({ type: "status", state: "working" });
    this.emit({ type: "assistant_text", message_id: "f1", block_index: 1, text: "" });
    this.emit({ type: "turn_end", is_error: true, result: "Failed to authenticate. API Error: 401 Invalid bearer token", cost_usd: 0, duration_ms: 1_830 });
    this.emit({ type: "status", state: "idle" });
    this.log("Colony stopped", "warn");
  }

  addFrame(frame: object): void {
    this.frames.push({ afterSeq: this.seq, frame });
    this.broadcast(frame);
  }

  /** A running colony stuck on a watch-mode command that the watchdog has nudged once. */
  seedStalled(proposal: MemoryProposal): void {
    this.started = true;
    const s = this.session;
    this.log("Worktree created, microVM booted, joined mesh");
    this.emit({ type: "model_changed", model: this.model, previous: null }, ago(27));
    this.emit({ type: "status", state: "working" }, ago(27));
    this.emit(
      {
        type: "user_message",
        id: "initial",
        text: `Resolve GitHub issue #${s.issue} in ${s.repo}: ${s.issue_title}.\n\nRead the relevant code, make a focused fix with tests, and ask before making product decisions.`,
      },
      ago(27),
    );
    this.emit({ type: "assistant_text", message_id: "s1", block_index: 0, text: "I'll look at how the order confirmation email is built first." }, ago(26));
    this.emit({ type: "tool_call", message_id: "s1", tool_call_id: "toolu_s1", name: "Read", input: { file_path: "/workspace/emails/order-confirmation.mjml" } }, ago(26));
    this.emit(
      {
        type: "tool_result",
        tool_call_id: "toolu_s1",
        output: '<mjml>\n  <mj-head>\n    <mj-attributes>\n      <mj-all font-family="Inter, Arial" />\n    </mj-attributes>\n  </mj-head>\n  <mj-body background-color="#ffffff">',
        is_error: false,
      },
      ago(25),
    );
    this.emit(
      {
        type: "assistant_text",
        message_id: "s2",
        block_index: 0,
        text: "The templates are MJML, compiled by `npm run build:emails`. That isn't written down anywhere, so I proposed a repository note. Next I'll add a `prefers-color-scheme` block and rebuild.",
      },
      ago(21),
    );
    this.addFrame({ type: "memory_proposed", proposal });
    this.emit(
      {
        type: "tool_call",
        message_id: "s2",
        tool_call_id: "toolu_s2",
        name: "Bash",
        input: { command: "npm run build:emails -- --watch", description: "Rebuild email templates" },
      },
      ago(19),
    );
    this.log("provider strix is unreachable (connect timed out after 5 s); strix/ds4-flash requests fall back to sonnet", "warn");
    this.log("watchdog: no progress for 15 min, nudged the agent (1/3)", "warn");
    this.emit(
      {
        type: "user_message",
        id: "watchdog-1",
        text: "Watchdog check: this colony has shown no progress for 15 minutes. If a command or process is hanging, stop it and try another way. If you need a decision from the maintainer, ask with a choice card. Otherwise, continue the task and report what you're doing.",
      },
      ago(4),
    );
    s.last_activity_at = ago(19);
    s.attention = { reason: "stalled", since: ago(19), nudges: 1 };
  }

  private async intro(): Promise<void> {
    const generation = this.generation;
    const s = this.session;
    if (s.issue == null) {
      if (s.status === "starting") {
        this.log(`Booting microVM ${s.sandbox} for a new colony`);
        await sleep(1200);
        this.patch({ status: "running" });
      }
      this.emit({ type: "model_changed", model: this.model, previous: null });
      if (this.instructions) this.emit({ type: "user_message", id: "initial", text: `Colony on ${s.repo}.\n\n${this.instructions}` });
      this.emit({ type: "status", state: "working" });
      await sleep(500);
      if (!(await this.streamText(generation, "msg_open", `I'm ready in \`/workspace\` on branch \`${s.branch}\`. What should I work on?`))) return;
      this.emit({
        type: "turn_end",
        is_error: false,
        result: null,
        cost_usd: 0.01,
        duration_ms: 2_100,
        model_usage: this.turnUsage({ "claude-opus-5": { input_tokens: 1_850, output_tokens: 240, cache_read_tokens: 19_400, cache_write_tokens: 1_200, thinking_tokens: 0 } }),
      });
      this.emit({ type: "status", state: "idle" });
      return;
    }
    if (s.status === "starting") {
      this.log(`Creating worktree ${s.branch} from origin/${s.base ?? "main"}`);
      await sleep(700);
      this.log(`Booting microVM ${s.sandbox} (node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0, 4 vCPU, 8G)`);
      await sleep(900);
      this.log(`Joined the private mesh as ${s.mesh?.name} (${s.mesh?.ip}) — direct connection`);
      this.patch({ status: "running" });
    } else {
      this.log(`Worktree ${s.branch} created from origin/${s.base ?? "main"}`);
      this.log(`microVM ${s.sandbox} booted (node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0, 4 vCPU, 8G)`);
      this.log(`Joined the private mesh as ${s.mesh?.name} (${s.mesh?.ip}) — direct connection`);
    }
    this.emit({ type: "model_changed", model: this.model, previous: null });
    this.emit({ type: "status", state: "working" });
    this.emit({
      type: "user_message",
      id: "initial",
      text: `Resolve GitHub issue #${s.issue} in ${s.repo}: ${s.issue_title}.\n\nRead the relevant code, make a focused fix with tests, and ask before making product decisions.`,
    });
    await sleep(600);
    if (
      !(await this.streamText(
        generation,
        "msg_1",
        "I'll send three settlers out at once: one to find where guest checkout fails, one to run the checkout suite, and one to review what changed in checkout lately.",
      ))
    )
      return;
    // The orchestrator delegates, as colonies do: Task calls, then the settlers' own events, interleaved while they
    // work in parallel, then each one's report as its Task result.
    const scout: AgentRef = { id: "toolu_task1", name: "Explore", description: "Find where guest checkout fails" };
    const tester: AgentRef = { id: "toolu_task2", name: "test-runner", description: "Run the checkout suite" };
    const inspector: AgentRef = { id: "toolu_task3", name: "code-reviewer", description: "Review recent checkout changes" };
    const prompts: [AgentRef, string][] = [
      [scout, "Find where guest checkout fails. Report conclusions with file:line references."],
      [tester, "Run the checkout tests and report which fail and why."],
      [inspector, "Review the last few commits touching src/checkout for anything that explains guest checkout failing."],
    ];
    for (const [agent, prompt] of prompts) {
      this.emit({
        type: "tool_call",
        message_id: "msg_1",
        tool_call_id: agent.id,
        name: "Task",
        input: { subagent_type: agent.name, description: agent.description, prompt },
      });
    }
    const finish = async (agent: AgentRef, messageId: string, report: string): Promise<boolean> => {
      if (!(await this.streamText(generation, messageId, report, agent))) return false;
      await sleep(200);
      if (!this.alive(generation)) return false;
      this.emit({ type: "tool_result", tool_call_id: agent.id, output: report, is_error: false });
      return true;
    };
    const settlers = await Promise.all([
      (async () => {
        await sleep(500);
        if (!this.alive(generation)) return false;
        this.emit({ type: "thinking", message_id: "sub_s1", block_index: 0, text: "Guest checkout failing — search for guest handling first.", agent: scout });
        await sleep(1400);
        if (
          !(await this.tool(
            generation,
            "sub_s1",
            "toolu_s1a",
            "Bash",
            { command: 'grep -rn "guest" src/checkout --include=*.ts', description: "Find guest checkout code" },
            'src/checkout/session.ts:5:  if (!user) throw new Error("guest checkout disabled");\nsrc/checkout/cart.ts:12:export async function createGuestCart(email: string) {\nsrc/checkout/api.ts:71:  const cart = await createGuestCart(body.email);',
            1800,
            scout,
          ))
        )
          return false;
        if (!(await this.tool(generation, "sub_s1", "toolu_s1b", "Read", { file_path: "/workspace/src/checkout/session.ts" }, SESSION_TS, 1600, scout)))
          return false;
        await sleep(1200);
        return finish(
          scout,
          "sub_s2",
          "`createSession()` throws when nobody is signed in (`src/checkout/session.ts:5`), so a guest never reaches `createGuestCart()` (`src/checkout/cart.ts:12`), which `src/checkout/api.ts:71` would otherwise call.",
        );
      })(),
      (async () => {
        await sleep(800);
        if (!this.alive(generation)) return false;
        this.emit({ type: "thinking", message_id: "sub_t1", block_index: 0, text: "Run the checkout tests as they are.", agent: tester });
        await sleep(700);
        if (
          !(await this.tool(
            generation,
            "sub_t1",
            "toolu_t1a",
            "Bash",
            { command: "npm test -- checkout", description: "Run checkout tests" },
            "FAIL  test/checkout.guest.test.ts\n  ✕ lets guests check out (31 ms)\n    Error: guest checkout disabled\n      at createSession (src/checkout/session.ts:5:21)\n\nTests:       2 failed, 12 passed, 14 total",
            2400,
            tester,
            true,
          ))
        )
          return false;
        if (!(await this.tool(generation, "sub_t1", "toolu_t1b", "Read", { file_path: "/workspace/test/checkout.guest.test.ts" }, GUEST_TEST, 1500, tester)))
          return false;
        await sleep(900);
        return finish(
          tester,
          "sub_t2",
          "2 of 14 checkout tests fail, both in `test/checkout.guest.test.ts`: each dies with `guest checkout disabled`, thrown from `createSession()` before a cart exists.",
        );
      })(),
      (async () => {
        await sleep(1100);
        if (!this.alive(generation)) return false;
        this.emit({ type: "thinking", message_id: "sub_i1", block_index: 0, text: "Look at recent commits under src/checkout.", agent: inspector });
        await sleep(900);
        if (
          !(await this.tool(
            generation,
            "sub_i1",
            "toolu_i1a",
            "Bash",
            { command: "git log --oneline -n 5 -- src/checkout", description: "Recent checkout commits" },
            "a41c2e9 Let guests start a cart from the checkout form\n7be0d13 Move session handling into session.ts\n2f9a8c1 Validate the email on guest orders",
            1300,
            inspector,
          ))
        )
          return false;
        if (!(await this.tool(generation, "sub_i1", "toolu_i1b", "Read", { file_path: "/workspace/src/checkout/api.ts" }, "// api.ts (excerpt)", 2000, inspector)))
          return false;
        await sleep(1500);
        return finish(
          inspector,
          "sub_i2",
          "`a41c2e9` added the guest path in `src/checkout/api.ts`, but `7be0d13` had already moved session handling into `session.ts`, which still rejects anyone not signed in. The two commits never met in a test.",
        );
      })(),
    ]);
    if (settlers.includes(false)) return;
    await sleep(300);
    if (
      !(await this.streamText(
        generation,
        "msg_2",
        "Found it: `createSession()` throws whenever there is no signed-in user, so the **guest path never reaches `createGuestCart()`**.\n\nThere are a few reasonable fixes and they change what the product does, so I'd like your call before I edit anything.",
      ))
    )
      return;
    this.pendingQuestion = "toolu_q1";
    this.emit({ type: "status", state: "waiting_for_answer" });
    this.emit({ type: "question", question_id: "toolu_q1", message_id: "msg_2", questions: DEMO_QUESTIONS });
  }

  private async onAnswer(questionId: string, answers: Answers, response: string | null): Promise<void> {
    this.pendingQuestion = null;
    const generation = ++this.generation;
    await sleep(350);
    this.emit({ type: "question_answered", question_id: questionId, answers, response });
    this.emit({ type: "status", state: "working" });

    const flow = answers[DEMO_QUESTIONS[0].question];
    const scopeValue = answers[DEMO_QUESTIONS[1].question];
    const flowText = Array.isArray(flow) ? flow.join(", ") : (flow ?? response ?? "your answer");
    const scope = Array.isArray(scopeValue) ? scopeValue : scopeValue ? [scopeValue] : [];
    const withTests = scope.some((item) => /test/i.test(item));
    const scopeText = scope.length ? `, and I'll include ${scope.map((x) => `*${x.toLowerCase()}*`).join(" and ")}` : "";

    if (!(await this.streamText(generation, "msg_3", `Going with **${flowText}**${scopeText}.`))) return;
    if (
      !(await this.tool(
        generation,
        "msg_3",
        "toolu_3",
        "Edit",
        {
          file_path: "/workspace/src/checkout/session.ts",
          old_string: 'if (!user) throw new Error("guest checkout disabled");',
          new_string: "if (!user) return { user: null, cart: await createGuestCart(req.body.email) };",
        },
        "The file /workspace/src/checkout/session.ts has been updated successfully.",
      ))
    )
      return;
    if (withTests) {
      if (
        !(await this.tool(
          generation,
          "msg_3",
          "toolu_4",
          "Bash",
          { command: "npm test -- checkout", description: "Run checkout tests" },
          "PASS  test/checkout.guest.test.ts\n  ✓ lets guests check out (212 ms)\n  ✓ keeps the guest email on the order (48 ms)\n\nTest Suites: 3 passed, 3 total\nTests:       14 passed, 14 total",
          1400,
        ))
      )
        return;
    }
    const summary = [
      "Guest checkout works again.",
      "",
      "- `createSession()` falls back to a guest cart when nobody is signed in",
      "- The order keeps the email address the guest entered",
      withTests ? "- Added `test/checkout.guest.test.ts` — all 14 checkout tests pass" : null,
      "",
      "Press **Create PR** when you're happy, or tell me what to change.",
    ]
      .filter((line) => line !== null)
      .join("\n");
    if (!(await this.streamText(generation, "msg_4", summary))) return;
    this.cost += 0.42;
    // First result of the run: it covers the delegation turn, the settlers' routed-model work and this turn, so
    // the colony-cumulative total is all new and both models belong to this footer. A later turn that only the
    // orchestrator served (see onUserMessage) must then diff down to its model alone.
    this.emit({
      type: "turn_end",
      is_error: false,
      result: "Guest checkout fixed.",
      cost_usd: Math.round(this.cost * 100) / 100,
      duration_ms: 81_234,
      model_usage: this.turnUsage({
        "claude-opus-5": { input_tokens: 46_200, output_tokens: 9_100, cache_read_tokens: 402_000, cache_write_tokens: 16_800, thinking_tokens: 0 },
        "deepseek/deepseek-flash": { input_tokens: 28_400, output_tokens: 6_200, cache_read_tokens: 0, cache_write_tokens: 0, thinking_tokens: 0 },
      }),
    });
    this.emit({ type: "status", state: "idle" });
  }

  private async onUserMessage(text: string): Promise<void> {
    const generation = ++this.generation;
    const n = ++this.userMessages;
    await sleep(250);
    this.emit({ type: "user_message", id: `u-${n}`, text });
    this.emit({ type: "status", state: "working" });
    const reply = [
      `Got it. *(mock reply)* In a real colony I'd now work on “${text.slice(0, 80)}${text.length > 80 ? "…" : ""}” inside the microVM.`,
      "",
      "Here is how I would go about it:",
      "",
      "1. **Read** `src/checkout/guest.ts` and the session middleware, to see where a guest gets its cart.",
      "2. **Reproduce** the failure with a test that checks out without signing in.",
      "3. **Fix** the smallest thing that makes that test pass, and keep the signed-in path unchanged.",
      "",
      "The likely culprit is the cart lookup, which assumes a user id:",
      "",
      "```ts",
      "const cart = await carts.findByUser(session.userId!);",
      "```",
      "",
      "For a guest `session.userId` is undefined, so the lookup throws before payment starts. I'd look the cart up by session id instead when there is no user, and add a test for both paths.",
    ].join("\n");
    if (!(await this.streamText(generation, `msg_u${n}`, reply))) return;
    this.cost += 0.06;
    // Only the orchestrator ran this turn; the settlers' routed tokens stay in the cumulative total but must
    // not be attributed here.
    this.emit({
      type: "turn_end",
      is_error: false,
      result: reply,
      cost_usd: Math.round(this.cost * 100) / 100,
      duration_ms: 4_210,
      model_usage: this.turnUsage({ [this.model]: { input_tokens: 5_800, output_tokens: 940, cache_read_tokens: 48_000, cache_write_tokens: 3_600, thinking_tokens: 0 } }),
    });
    this.emit({ type: "status", state: this.pendingQuestion ? "waiting_for_answer" : "idle" });
  }

  private seedHistory(): void {
    this.started = true;
    this.log("Worktree created, microVM booted, joined mesh");
    this.emit({ type: "user_message", id: "initial", text: `Resolve GitHub issue #${this.session.issue} in ${this.session.repo}: ${this.session.issue_title}.` });
    this.emit({ type: "assistant_text", message_id: "h1", block_index: 0, text: "Cart totals were summed as floats. I switched the cart to integer cents and added tests." });
    this.emit({ type: "turn_end", is_error: false, result: "Done", cost_usd: 1.12, duration_ms: 214_000 });
    this.log("Committed 4 files, pushed, opened pull request");
  }
}

export const RESPONSES: Record<string, string> = {
  ls: "README.md  node_modules  package.json  src  test\r\n",
  pwd: "/workspace\r\n",
  whoami: "root\r\n",
  "git status":
    "On branch colonizer/issue-42-demo1234\r\nChanges not staged for commit:\r\n  \x1b[31mmodified:   src/checkout/session.ts\x1b[0m\r\n",
  "tailscale status":
    "100.64.0.3  colony-demo1234  vms      linux  -\r\n100.64.0.1  mothership       harness  linux  active; direct 192.168.1.6:41981\r\n",
};

export function mockTerminal(session: MockSession | undefined): SocketLike {
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const bytes = (text: string): ArrayBuffer => encoder.encode(text).slice().buffer;
  const host = session?.session.sandbox ?? "colony-mock";
  const prompt = `\x1b[1;32mroot@${host}\x1b[0m:\x1b[1;34m/workspace\x1b[0m# `;
  let line = "";
  const socket = new MockSocket({
    open: (s) => {
      if (!session || !isLive(session.session.status)) {
        s.deliver(JSON.stringify({ type: "exit", code: 1 }));
        setTimeout(() => s.close(), 30);
        return;
      }
      s.deliver(bytes(`\x1b[2mColonizer mock terminal — commands are simulated.\x1b[0m\r\n\r\n${prompt}`));
    },
    message: (s, data) => {
      if (typeof data === "string") return; // resize
      const text = decoder.decode(data instanceof ArrayBuffer ? new Uint8Array(data) : (data as Uint8Array));
      let out = "";
      for (const ch of text) {
        if (ch === "\r") {
          const command = line.trim();
          line = "";
          if (command === "exit") {
            s.deliver(bytes(`${out}\r\nlogout\r\n`));
            s.deliver(JSON.stringify({ type: "exit", code: 0 }));
            setTimeout(() => s.close(), 30);
            return;
          }
          const response = command ? (RESPONSES[command] ?? `mock: ${command.split(/\s+/)[0]}: not simulated in mock mode\r\n`) : "";
          out += `\r\n${response}${prompt}`;
        } else if (ch === "\x7f") {
          if (line) {
            line = line.slice(0, -1);
            out += "\b \b";
          }
        } else if (ch === "\x03") {
          line = "";
          out += `^C\r\n${prompt}`;
        } else if (ch >= " ") {
          line += ch;
          out += ch;
        }
      }
      if (out) s.deliver(bytes(out));
    },
    close: () => {},
  });
  return socket as unknown as SocketLike;
}

export function baseSession(id: string, repo: string, issue: number | null, title: string): Session {
  const slug = issue != null ? `issue-${issue}-${id}` : `session-${id}`;
  return {
    id,
    repo,
    org: repo.split("/")[0],
    last_activity_at: now(),
    attention: null,
    issue,
    issue_title: title,
    // A short, plain task line like the one the mothership's cheap model writes.
    summary: title.length > 60 ? `${title.slice(0, 57).trimEnd()}…` : title,
    status: "starting",
    branch: `colonizer/${slug}`,
    base: "main",
    parent: null,
    worktree: `/home/you/.local/share/colonizer/worktrees/${repo}/${slug}`,
    git_admin_dir: `/home/you/.local/share/colonizer/repos/${repo}.git/worktrees/${slug}`,
    sandbox: `colony-${id}`,
    mesh: { name: `colony-${id}`, ip: `100.64.0.${Math.floor(Math.random() * 200) + 10}` },
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    keep_worktree: false,
    created_at: now(),
    updated_at: now(),
  };
}
