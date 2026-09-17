// In-browser mock of the harness API and event streams, enabled with `?mock=1`.
import { ApiError, type Api, type SocketLike } from "./api";
import type {
  AgentRef,
  AgentEvent,
  HeadroomStatus,
  AgentEventBody,
  Answers,
  HarnessStatus,
  Issue,
  LogLevel,
  LoginView,
  MemoryNote,
  MemoryProposal,
  ModelOption,
  ModelProvider,
  ModuleInfo,
  OrgInfo,
  OrgSettings,
  PullStatus,
  Question,
  Repo,
  Session,
  SessionStatus,
} from "./types";

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
const now = () => new Date().toISOString();
const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();
const clone = <T>(value: T): T => structuredClone(value);
const LIVE: SessionStatus[] = ["starting", "running", "waiting_for_answer", "idle"];
const isLive = (status: SessionStatus) => LIVE.includes(status);

interface SocketHandlers {
  open(socket: MockSocket): void;
  message(socket: MockSocket, data: unknown): void;
  close(socket: MockSocket): void;
}

class MockSocket {
  binaryType: BinaryType = "blob";
  readyState = 0;
  onopen: ((event: Event) => unknown) | null = null;
  onmessage: ((event: MessageEvent) => unknown) | null = null;
  onclose: ((event: CloseEvent) => unknown) | null = null;
  onerror: ((event: Event) => unknown) | null = null;
  private readonly handlers: SocketHandlers;

  constructor(handlers: SocketHandlers) {
    this.handlers = handlers;
    setTimeout(() => {
      if (this.readyState !== 0) return;
      this.readyState = 1;
      this.onopen?.(new Event("open"));
      this.handlers.open(this);
    }, 150);
  }

  deliver(data: string | ArrayBuffer): void {
    if (this.readyState === 1) this.onmessage?.(new MessageEvent("message", { data }));
  }

  send(data: unknown): void {
    if (this.readyState === 1) this.handlers.message(this, data);
  }

  close(): void {
    if (this.readyState >= 2) return;
    this.readyState = 3;
    this.handlers.close(this);
    this.onclose?.(new CloseEvent("close", { code: 1000 }));
  }
}

const DEMO_QUESTIONS: Question[] = [
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

const SESSION_TS = `import { createGuestCart } from "./cart";

export async function createSession(req: Request) {
  const user = await currentUser(req);
  if (!user) throw new Error("guest checkout disabled");
  return { user, cart: await loadCart(user.id) };
}`;

class MockSession {
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
    let command: { type?: string; text?: string; question_id?: string; answers?: Answers; response?: string | null };
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
    }
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
    for (let i = 0; i < words.length; i += 2) {
      if (!this.alive(generation)) return false;
      this.emit({ type: "assistant_text_delta", message_id: messageId, block_index: 0, delta: words.slice(i, i + 2).join(""), ...by });
      await sleep(40);
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
  ): Promise<boolean> {
    if (!this.alive(generation)) return false;
    const by = agent ? { agent } : {};
    this.emit({ type: "tool_call", message_id: messageId, tool_call_id: id, name, input, ...by });
    await sleep(delay);
    if (!this.alive(generation)) return false;
    this.emit({ type: "tool_result", tool_call_id: id, output, is_error: false, ...by });
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
      if (this.instructions) this.emit({ type: "user_message", id: "initial", text: `Colony on ${s.repo}.\n\n${this.instructions}` });
      this.emit({ type: "status", state: "working" });
      await sleep(500);
      if (!(await this.streamText(generation, "msg_open", `I'm ready in \`/workspace\` on branch \`${s.branch}\`. What should I work on?`))) return;
      this.emit({ type: "turn_end", is_error: false, result: null, cost_usd: 0.01, duration_ms: 2_100 });
      this.emit({ type: "status", state: "idle" });
      return;
    }
    if (s.status === "starting") {
      this.log(`Creating worktree ${s.branch} from origin/${s.base ?? "main"}`);
      await sleep(700);
      this.log(`Booting microVM ${s.sandbox} (node:24-bookworm, 4 vCPU, 8G)`);
      await sleep(900);
      this.log(`Joined the private mesh as ${s.mesh?.name} (${s.mesh?.ip}) — direct connection`);
      this.patch({ status: "running" });
    } else {
      this.log(`Worktree ${s.branch} created from origin/${s.base ?? "main"}`);
      this.log(`microVM ${s.sandbox} booted (node:24-bookworm, 4 vCPU, 8G)`);
      this.log(`Joined the private mesh as ${s.mesh?.name} (${s.mesh?.ip}) — direct connection`);
    }
    this.emit({ type: "status", state: "working" });
    this.emit({
      type: "user_message",
      id: "initial",
      text: `Resolve GitHub issue #${s.issue} in ${s.repo}: ${s.issue_title}.\n\nRead the relevant code, make a focused fix with tests, and ask before making product decisions.`,
    });
    await sleep(600);
    if (!(await this.streamText(generation, "msg_1", "I'll send a scout to find where guest checkout is handled before deciding anything."))) return;
    // The orchestrator delegates the reading, as colonies do: a Task call, the subagent's own events, then its report.
    const scout: AgentRef = { id: "toolu_task1", name: "Explore", description: "Find where guest checkout fails" };
    const report =
      "`createSession()` throws when nobody is signed in (`src/checkout/session.ts:5`), so a guest never reaches `createGuestCart()` (`src/checkout/cart.ts:12`), which `src/checkout/api.ts:71` would otherwise call.";
    this.emit({
      type: "tool_call",
      message_id: "msg_1",
      tool_call_id: scout.id,
      name: "Task",
      input: { subagent_type: "Explore", description: scout.description, prompt: "Find where guest checkout fails. Report conclusions with file:line references." },
    });
    await sleep(500);
    if (!this.alive(generation)) return;
    this.emit({ type: "thinking", message_id: "sub_1", block_index: 0, text: "Guest checkout failing — search for guest handling first.", agent: scout });
    await sleep(1400);
    if (
      !(await this.tool(
        generation,
        "sub_1",
        "toolu_s1a",
        "Bash",
        { command: 'grep -rn "guest" src/checkout --include=*.ts', description: "Find guest checkout code" },
        'src/checkout/session.ts:5:  if (!user) throw new Error("guest checkout disabled");\nsrc/checkout/cart.ts:12:export async function createGuestCart(email: string) {\nsrc/checkout/api.ts:71:  const cart = await createGuestCart(body.email);',
        1800,
        scout,
      ))
    )
      return;
    if (!(await this.tool(generation, "sub_1", "toolu_s1b", "Read", { file_path: "/workspace/src/checkout/session.ts" }, SESSION_TS, 1600, scout))) return;
    await sleep(1200);
    if (!(await this.streamText(generation, "sub_2", report, scout))) return;
    await sleep(300);
    if (!this.alive(generation)) return;
    this.emit({ type: "tool_result", tool_call_id: scout.id, output: report, is_error: false });
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
    this.emit({ type: "turn_end", is_error: false, result: "Guest checkout fixed.", cost_usd: Math.round(this.cost * 100) / 100, duration_ms: 81_234 });
    this.emit({ type: "status", state: "idle" });
  }

  private async onUserMessage(text: string): Promise<void> {
    const generation = ++this.generation;
    const n = ++this.userMessages;
    await sleep(250);
    this.emit({ type: "user_message", id: `u-${n}`, text });
    this.emit({ type: "status", state: "working" });
    const reply = `Got it. *(mock reply)* In a real colony I'd now work on “${text.slice(0, 80)}${text.length > 80 ? "…" : ""}” inside the microVM.`;
    if (!(await this.streamText(generation, `msg_u${n}`, reply))) return;
    this.cost += 0.06;
    this.emit({ type: "turn_end", is_error: false, result: reply, cost_usd: Math.round(this.cost * 100) / 100, duration_ms: 4_210 });
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

const RESPONSES: Record<string, string> = {
  ls: "README.md  node_modules  package.json  src  test\r\n",
  pwd: "/workspace\r\n",
  whoami: "root\r\n",
  "git status":
    "On branch colonizer/issue-42-demo1234\r\nChanges not staged for commit:\r\n  \x1b[31mmodified:   src/checkout/session.ts\x1b[0m\r\n",
  "tailscale status":
    "100.64.0.3  colony-demo1234  vms      linux  -\r\n100.64.0.1  mothership       harness  linux  active; direct 192.168.1.6:41981\r\n",
};

function mockTerminal(session: MockSession | undefined): SocketLike {
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

const REPOS: Repo[] = [
  { full_name: "acme/webshop", description: "Storefront and checkout", private: true, fork: false, archived: false, open_issues_count: 2, pushed_at: ago(30) },
  { full_name: "acme/design-system", description: "Shared UI components", private: false, fork: false, archived: false, open_issues_count: 5, pushed_at: ago(600) },
  { full_name: "octocat/hello-world", description: null, private: false, fork: true, archived: false, open_issues_count: 0, pushed_at: ago(9000) },
];

const ISSUES: Record<string, Issue[]> = {
  "acme/webshop": [
    {
      number: 42,
      title: "Checkout fails for guest users",
      body: 'Guests get "Something went wrong" when pressing **Pay**. Logged-in users are fine.\n\nSteps:\n1. Open the shop in a private window\n2. Add any item\n3. Checkout without signing in',
      labels: [
        { name: "bug", color: "d73a4a" },
        { name: "checkout", color: "0e8a16" },
      ],
      author: { login: "maria" },
      updatedAt: ago(90),
      url: "https://github.com/acme/webshop/issues/42",
    },
    {
      number: 43,
      title: "Add dark mode to the order confirmation email",
      body: "The confirmation email is unreadable in dark-mode mail clients.",
      labels: [{ name: "enhancement", color: "a2eeef" }],
      author: { login: "sam" },
      updatedAt: ago(1500),
      url: "https://github.com/acme/webshop/issues/43",
    },
  ],
  "acme/design-system": [
    {
      number: 7,
      title: "Button focus ring is invisible on dark backgrounds",
      body: null,
      labels: [{ name: "a11y", color: "5319e7" }],
      author: { login: "lee" },
      updatedAt: ago(300),
      url: "https://github.com/acme/design-system/issues/7",
    },
  ],
};

function baseSession(id: string, repo: string, issue: number | null, title: string): Session {
  const slug = issue != null ? `issue-${issue}-${id}` : `session-${id}`;
  return {
    id,
    repo,
    org: repo.split("/")[0],
    last_activity_at: now(),
    attention: null,
    issue,
    issue_title: title,
    status: "starting",
    branch: `colonizer/${slug}`,
    base: "main",
    worktree: `/home/you/.local/share/colonizer/worktrees/${repo}/${slug}`,
    sandbox: `colony-${id}`,
    mesh: { name: `colony-${id}`, ip: `100.64.0.${Math.floor(Math.random() * 200) + 10}` },
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    created_at: now(),
    updated_at: now(),
  };
}

const MOCK_PRESET_IMAGES: Record<string, string> = {
  node: "node:24-bookworm",
  python: "python:3.13-bookworm",
  rust: "rust:1-bookworm",
  go: "golang:1-bookworm",
};
const mockPulled = new Set<string>(["node:24-bookworm"]);
let mockPull: PullStatus = { image: "", state: "idle", started_at: null, finished_at: null, error: null };
// Headroom's bundle: a few seconds of download progress, then installed.
let mockHeadroom: HeadroomStatus = { release: "0.37.0-1", state: "idle", bytes: 0, total: null, started_at: null, finished_at: null, error: null };

export function createMockApi(): Api {
  const sessions = new Map<string, MockSession>();
  const demo = new MockSession({
    ...baseSession("demo1234", "acme/webshop", 42, "Checkout fails for guest users"),
    status: "running",
    mesh: { name: "colony-demo1234", ip: "100.64.0.3" },
    created_at: ago(6),
  });
  const old = new MockSession(
    {
      ...baseSession("old98765", "acme/webshop", 37, "Price rounding in cart totals"),
      status: "pr_opened",
      mesh: null,
      pr_url: "https://github.com/acme/webshop/pull/61",
      cost_usd: 1.12,
      created_at: ago(1600),
      updated_at: ago(1560),
    },
    true,
  );
  old.session.updated_at = ago(1560);
  const failed = new MockSession({
    ...baseSession("fail4321", "acme/design-system", 7, "Button focus ring is invisible on dark backgrounds"),
    status: "stopped",
    mesh: null,
    created_at: ago(95),
  });
  failed.seedFailedHistory();
  failed.session.updated_at = ago(93);
  sessions.set(demo.session.id, demo);
  sessions.set(failed.session.id, failed);
  sessions.set(old.session.id, old);

  // Shared memory: two proposals waiting for review and a few notes per scope.
  const proposals: MemoryProposal[] = [
    {
      id: "prop-emails",
      scope: "repo",
      key: "acme/webshop",
      title: "Build email templates with `npm run build:emails`",
      content:
        "Order and account emails live in `emails/*.mjml` and compile to `dist/emails/*.html`.\n\n- Run `npm run build:emails` once after editing; **don't** pass `--watch` in a colony, it never exits\n- Snapshot tests: `npm test -- emails`",
      tags: ["build", "emails"],
      created_at: ago(21),
      source: { session_id: "stall5678", repo: "acme/webshop" },
      status: "pending",
    },
    {
      id: "prop-pnpm",
      scope: "org",
      key: "acme",
      title: "Use pnpm in acme repositories",
      content: "Every acme repository has a `pnpm-lock.yaml`. Use `pnpm install` and `pnpm run <script>`; `npm install` creates a second lockfile that CI rejects.",
      tags: ["tooling"],
      created_at: ago(3),
      source: { session_id: "demo1234", repo: "acme/webshop" },
      status: "pending",
    },
  ];
  const notes: MemoryNote[] = [
    {
      id: "note-g1",
      scope: "global",
      key: "",
      title: "Ask before adding dependencies",
      content: "Prefer the standard library and what the repository already uses. Ask with a choice card before adding a new package.",
      tags: [],
      created_at: ago(8000),
      source: { user: true },
    },
    {
      id: "note-g2",
      scope: "global",
      key: "",
      title: "Commit messages",
      content: "Imperative mood, under 72 characters, no trailing period. Reference the issue in the body, not the subject.",
      tags: ["git"],
      created_at: ago(7000),
      source: { user: true },
    },
    {
      id: "note-o1",
      scope: "org",
      key: "acme",
      title: "Design tokens come from acme/design-system",
      content: "Never hard-code colours. Import tokens from `@acme/tokens`; dark mode values are under `tokens.dark`.",
      tags: ["ui"],
      created_at: ago(2400),
      source: { session_id: "old98765", repo: "acme/webshop" },
    },
    {
      id: "note-o2",
      scope: "org",
      key: "acme",
      title: "Staging deploys",
      content: "Merges to `main` deploy to staging automatically. Production needs a tagged release; colonies should not tag.",
      tags: [],
      created_at: ago(3000),
      source: { user: true },
    },
    {
      id: "note-r1",
      scope: "repo",
      key: "acme/webshop",
      title: "Prices are integer cents",
      content: "Cart and order totals are stored as integer cents (`amount_cents`). Format with `formatPrice()` from `src/money.ts`.",
      tags: ["checkout"],
      created_at: ago(1560),
      source: { session_id: "old98765", repo: "acme/webshop" },
    },
    {
      id: "note-r2",
      scope: "repo",
      key: "acme/webshop",
      title: "Checkout tests need the Stripe mock",
      content: "Start it with `pnpm stripe:mock` before `pnpm test -- checkout`, or the payment tests time out.",
      tags: ["tests"],
      created_at: ago(900),
      source: { user: true },
    },
    {
      id: "note-r3",
      scope: "repo",
      key: "acme/design-system",
      title: "Storybook is the source of truth",
      content: "Every component change needs an updated story; visual tests run against Storybook.",
      tags: [],
      created_at: ago(5000),
      source: { user: true },
    },
  ];

  // A running colony the watchdog nudged, and a colony in a second org.
  const stalled = new MockSession({
    ...baseSession("stall5678", "acme/webshop", 43, "Add dark mode to the order confirmation email"),
    status: "running",
    mesh: { name: "colony-stall5678", ip: "100.64.0.7" },
    created_at: ago(28),
  });
  stalled.seedStalled(proposals[0]);
  stalled.session.updated_at = ago(4);
  sessions.set(stalled.session.id, stalled);
  const octo = new MockSession({
    ...baseSession("octo2468", "octocat/hello-world", null, "Refresh the README examples"),
    status: "stopped",
    mesh: null,
    cost_usd: 0.18,
    created_at: ago(320),
    updated_at: ago(300),
  });
  octo.session.updated_at = ago(300);
  sessions.set(octo.session.id, octo);

  const DEFAULT_LIMITS = { timeout_secs: 600, max_concurrent: null, queue_timeout_secs: null, context_tokens: null, fallback_model: null };
  const providers: ModelProvider[] = [
    {
      id: "deepseek",
      name: "DeepSeek",
      base_url: "https://api.deepseek.com/anthropic",
      auth: "x-api-key",
      wire: "anthropic",
      has_key: true,
      models: ["deepseek-flash", "deepseek-v4-pro"],
      preset: "deepseek",
      ...DEFAULT_LIMITS,
      in_flight: 0,
      queued: 0,
    },
    {
      id: "strix",
      name: "Strix Halo",
      base_url: "http://strix.tail4c2e.ts.net:8080",
      auth: "none",
      wire: "anthropic",
      has_key: false,
      models: ["ds4-flash"],
      preset: "local",
      timeout_secs: 900,
      max_concurrent: 1,
      queue_timeout_secs: null,
      context_tokens: 131072,
      fallback_model: "sonnet",
      in_flight: 1,
      queued: 2,
    },
    {
      id: "lab",
      name: "Lab vLLM",
      base_url: "http://10.0.4.20:8000",
      auth: "bearer",
      wire: "anthropic",
      has_key: true,
      models: ["qwen3-coder"],
      preset: "custom",
      ...DEFAULT_LIMITS,
      max_concurrent: 4,
      in_flight: 0,
      queued: 0,
    },
  ];
  const LIMIT_RANGES: [keyof typeof DEFAULT_LIMITS, number, number][] = [
    ["timeout_secs", 30, 3600],
    ["max_concurrent", 1, 64],
    ["queue_timeout_secs", 1, 3600],
    ["context_tokens", 1024, 2_000_000],
  ];
  const ANTHROPIC_MODELS: [string, string][] = [
    ["opus", "Claude Opus (latest)"],
    ["sonnet", "Claude Sonnet (latest)"],
    ["haiku", "Claude Haiku (latest)"],
    ["fable", "Claude Fable (latest)"],
    ["claude-opus-5", "Claude Opus 5"],
    ["claude-sonnet-5", "Claude Sonnet 5"],
    ["claude-haiku-4-5", "Claude Haiku 4.5"],
  ];
  const orgSettings: Record<string, OrgSettings> = {
    acme: {
      agent: { model: "opus", subagent_model: "deepseek/deepseek-flash", background_model: null },
      max_parallel: 2,
      memory: { enabled: true },
      watchdog: { enabled: null, stall_minutes: 10, max_nudges: null },
    },
  };
  const orgOfKey = (note: MemoryNote) => (note.scope === "org" ? note.key : note.scope === "repo" ? note.key.split("/")[0] : null);

  let githubSource = "gh CLI login";
  let claude: HarnessStatus["claude"] = { configured: true, source: "Claude subscription", kind: "CLAUDE_CODE_OAUTH_TOKEN" };
  let login: LoginView = { state: "idle", url: null, message: null };
  const modules: ModuleInfo[] = [
    { kind: "source", provider: "github", providers: [{ id: "github", name: "GitHub", description: "Issues from repositories you can access" }], enabled: true, settings: {}, schema: null },
    {
      kind: "sandbox",
      provider: "microsandbox",
      providers: [{ id: "microsandbox", name: "microsandbox", description: "Rootless libkrun microVMs" }],
      enabled: true,
      settings: { image: "node:24-bookworm", cpus: 4, memory: "8G" },
      schema: {
        type: "object",
        properties: {
          image: { type: "string", title: "Image", description: "glibc-based OCI image", default: "node:24-bookworm" },
          cpus: { type: "integer", title: "vCPUs", minimum: 1, maximum: 64, default: 4 },
          memory: { type: "string", title: "Memory", default: "8G" },
          max_parallel: { type: "integer", title: "Parallel colonies", minimum: 1, maximum: 16, default: 3 },
        },
      },
    },
    {
      kind: "mesh",
      provider: "headscale",
      providers: [
        { id: "headscale", name: "Private mesh (Headscale)", description: "Bundled Headscale + Tailscale, separate from your own tailnet" },
        { id: "none", name: "Disabled", description: "Reach VMs through microsandbox only" },
      ],
      enabled: true,
      settings: { direct_udp: true },
      schema: {
        type: "object",
        properties: {
          direct_udp: { type: "boolean", title: "Direct connections", description: "Let VMs reach the Mothership node over UDP on this host", default: true },
        },
      },
    },
    {
      kind: "agent",
      provider: "claude-code",
      providers: [{ id: "claude-code", name: "Claude Code", description: "Claude Agent SDK runner" }],
      enabled: true,
      settings: { model: "", subagent_model: "", background_model: "", plugins: "ecc", caveman: false, caveman_level: "full", headroom: false, rtk: false },
      schema: {
        type: "object",
        properties: {
          model: { type: "string", title: "Orchestrator model", default: "", description: "Empty uses the Claude Code default" },
          subagent_model: { type: "string", title: "Subagent model", default: "", description: "e.g. deepseek/deepseek-flash; empty uses the orchestrator model" },
          background_model: { type: "string", title: "Background model", default: "", description: "Small, fast tasks; empty uses the Claude Code default" },
          plugins: {
            type: "string",
            format: "plugin-dirs",
            title: "Skillsets",
            description: "Claude Code plugin directories colonies load, read-only. All off by default.",
            default: "",
          },
          caveman: {
            type: "boolean",
            title: "Terse replies (caveman)",
            description: "Token savings: the agent answers in caveman's compressed style. Pull request descriptions and questions to you stay in plain sentences.",
            default: false,
          },
          caveman_level: { type: "string", enum: ["lite", "full", "ultra"], title: "Caveman level", description: "How terse, when terse replies are on.", default: "full" },
          headroom: {
            type: "boolean",
            title: "Compress what the agent reads (Headroom)",
            description: "Token savings: model requests pass through Headroom inside the colony. The first time it is switched on, the mothership downloads it (220–245 MB, depending on the architecture). It takes 300–370 MB of each colony’s memory.",
            default: false,
          },
          rtk: {
            type: "boolean",
            title: "Compact command output (rtk)",
            description: "Token savings: shell commands the agent runs go through rtk, which shortens their output.",
            default: false,
          },
        },
      },
    },
    {
      kind: "memory",
      provider: "files",
      providers: [{ id: "files", name: "Shared memory", description: "Markdown notes per repository, org and globally, mounted read-only into colonies; agents propose new notes" }],
      enabled: true,
      settings: { require_review: true },
      schema: {
        type: "object",
        properties: {
          require_review: {
            type: "boolean",
            title: "Review proposals before they become memory",
            description: "Recommended: an approved note becomes part of every future colony's context",
            default: true,
          },
        },
      },
    },
    {
      kind: "watchdog",
      provider: "default",
      providers: [{ id: "default", name: "Watchdog", description: "Nudges colonies that stop making progress and flags the ones that need you" }],
      enabled: true,
      settings: { stall_minutes: 15, max_nudges: 3, waiting_minutes: 30 },
      schema: {
        type: "object",
        properties: {
          stall_minutes: { type: "integer", title: "Nudge after minutes without progress", minimum: 1, maximum: 1440, default: 15 },
          max_nudges: { type: "integer", title: "Nudges before flagging", minimum: 0, maximum: 20, default: 3 },
          waiting_minutes: { type: "integer", title: "Flag unanswered questions after minutes", minimum: 1, maximum: 10080, default: 30 },
        },
      },
    },
    {
      kind: "interfaces",
      provider: "default",
      providers: [{ id: "default", name: "Colony panels" }],
      enabled: true,
      settings: { chat: true, terminal: true },
      schema: {
        type: "object",
        properties: {
          chat: { type: "boolean", title: "Chat", default: true },
          terminal: { type: "boolean", title: "Terminal", default: true },
        },
      },
    },
    {
      kind: "publish",
      provider: "github-pr",
      providers: [{ id: "github-pr", name: "GitHub pull request" }],
      enabled: true,
      settings: { autopilot: true, draft: false },
      schema: {
        type: "object",
        properties: {
          autopilot: { type: "boolean", title: "Open the PR automatically", default: true },
          draft: { type: "boolean", title: "Open PRs as drafts", default: false },
        },
      },
    },
  ];

  const find = (id: string): MockSession => {
    const session = sessions.get(id);
    if (!session) throw new ApiError("no such colony", 404);
    return session;
  };
  const later = async <T>(value: () => T, ms = 160): Promise<T> => {
    await sleep(ms);
    return clone(value());
  };

  return {
    mock: true,
    status: () =>
      later(() => ({
        github: { connected: true, login: "octocat", name: "The Octocat", source: githubSource },
        claude,
        sandbox: { msb_version: "msb 0.6.18", image: "node:24-bookworm", cpus: 4, memory: "8G", max_parallel: 3, claude_bin: "/opt/claude/bin/claude", claude_bin_error: null },
        mesh: {
          enabled: true,
          provider: "headscale",
          state: "running",
          harness_ip: "100.64.0.1",
          nodes: [...sessions.values()].filter((s) => isLive(s.session.status)).length + 1,
          error: null,
        },
      })),
    modules: () => later(() => modules),
    saveModule: async (kind, body) => {
      await sleep(250);
      const module = modules.find((m) => m.kind === kind);
      if (!module) throw new ApiError("unknown module kind", 404);
      if (!module.providers.some((p) => p.id === body.provider)) throw new ApiError("unknown provider", 400);
      Object.assign(module, { provider: body.provider, enabled: body.enabled, settings: body.settings });
      return clone(module);
    },
    // Simulates a cold pull that takes a few seconds, so the Settings row can be
    // seen going through pulling -> done against the mock backend.
    sandboxPull: async () => {
      const sandbox = modules.find((m) => m.kind === "sandbox");
      const preset = String(sandbox?.settings?.preset ?? "node");
      const image = String(sandbox?.settings?.image ?? MOCK_PRESET_IMAGES[preset] ?? "node:24-bookworm");
      if (mockPulled.has(image)) {
        mockPull = { image, state: "cached", started_at: null, finished_at: null, error: null };
        return clone(mockPull);
      }
      if (mockPull.state !== "pulling" || mockPull.image !== image) {
        mockPull = { image, state: "pulling", started_at: new Date().toISOString(), finished_at: null, error: null };
        const started = mockPull.started_at;
        setTimeout(() => {
          if (mockPull.image !== image || mockPull.started_at !== started) return;
          mockPulled.add(image);
          mockPull = { ...mockPull, state: "done", finished_at: new Date().toISOString() };
        }, 4000);
      }
      return clone(mockPull);
    },
    sandboxPullStatus: async () => clone(mockPull),
    headroom: async () => {
      if (mockHeadroom.state === "downloading" && mockHeadroom.started_at) {
        const total = 231_330_241;
        const bytes = Math.min(total, Math.round(((Date.now() - Date.parse(mockHeadroom.started_at)) / 5000) * total));
        mockHeadroom = bytes >= total ? { ...mockHeadroom, state: "installed", bytes, total, finished_at: new Date().toISOString() } : { ...mockHeadroom, bytes, total };
      }
      return clone(mockHeadroom);
    },
    headroomDownload: async () => {
      if (mockHeadroom.state === "idle" || mockHeadroom.state === "failed") {
        mockHeadroom = { ...mockHeadroom, state: "downloading", bytes: 0, total: 231_330_241, started_at: new Date().toISOString(), finished_at: null, error: null };
      }
      return clone(mockHeadroom);
    },
    repos: () => later(() => REPOS, 350),
    issues: (repo) => later(() => ISSUES[repo] ?? [], 300),
    sessions: () =>
      later(() => [...sessions.values()].map((s) => s.session).sort((a, b) => b.updated_at.localeCompare(a.updated_at))),
    session: async (id) => later(() => find(id).session),
    createSession: async (body) => {
      await sleep(450);
      const id = Math.random().toString(16).slice(2, 10);
      const issueNumber = body.issue ?? null;
      const issue = ISSUES[body.repo]?.find((i) => i.number === issueNumber);
      const title = issueNumber == null ? "Open colony" : (body.title ?? issue?.title ?? `Issue #${issueNumber}`);
      const session = new MockSession(
        { ...baseSession(id, body.repo, issueNumber, title), autopilot: body.autopilot ?? true },
        false,
        body.instructions ?? null,
      );
      sessions.set(id, session);
      return clone(session.session);
    },
    resumeSession: async (id) => {
      await sleep(250);
      const s = find(id);
      if (s.session.status !== "stopped" && s.session.status !== "failed") {
        throw new ApiError("this colony can't be resumed", 409);
      }
      s.patch({ status: "starting", error: null });
      return clone(s.session);
    },
    publishSession: async (id) => {
      const s = find(id);
      if (!isLive(s.session.status)) throw new ApiError("the colony is not running", 409);
      s.halt();
      s.patch({ status: "publishing" });
      s.log("Stopping the agent and removing the microVM");
      setTimeout(() => {
        s.log(`Committed 3 files on ${s.session.branch} and pushed`);
        s.patch({ status: "pr_opened", mesh: null, pr_url: `https://github.com/${s.session.repo}/pull/${60 + Math.floor(Math.random() * 40)}` });
        s.log("Opened pull request");
      }, 1800);
      return clone(s.session);
    },
    stopSession: async (id) => {
      const s = find(id);
      if (!isLive(s.session.status)) throw new ApiError("the colony is not running", 409);
      s.halt();
      s.patch({ status: "stopped", mesh: null });
      s.log("microVM stopped and removed; the worktree was kept");
      return clone(s.session);
    },
    deleteSession: async (id) => {
      const s = find(id);
      if (isLive(s.session.status) || s.session.status === "publishing") throw new ApiError("stop the colony first", 409);
      s.halt();
      sessions.delete(id);
      return { deleted: id, leftover: null };
    },
    cleanupSession: async (id) => {
      const s = find(id);
      if (isLive(s.session.status) || s.session.status === "publishing") throw new ApiError("stop the colony first", 409);
      s.patch({ cleaned_up: true });
      s.log("Removed the worktree and local branch");
      return clone(s.session);
    },
    setGithubToken: async (token) => {
      await sleep(300);
      if (!token.trim() || /\s/.test(token.trim())) throw new ApiError("empty or malformed token", 400);
      githubSource = "saved token";
      return { login: "octocat" };
    },
    deleteGithubToken: async () => {
      githubSource = "gh CLI login";
      return { ok: true };
    },
    setClaudeToken: async (token) => {
      if (!token.trim().startsWith("sk-ant-")) {
        throw new ApiError("expected a token from `claude setup-token` (sk-ant-oat…) or an API key (sk-ant-api…)", 400);
      }
      claude = { configured: true, source: token.includes("-api") ? "saved API key" : "Claude subscription", kind: "CLAUDE_CODE_OAUTH_TOKEN" };
      return { ok: true };
    },
    deleteClaudeToken: async () => {
      claude = { configured: false, source: null, kind: null };
      return { ok: true };
    },
    claudeLogin: () => later(() => login, 60),
    claudeLoginStart: async () => {
      login = { state: "starting", url: null, message: null };
      setTimeout(() => {
        if (login.state === "starting") {
          login = { state: "awaiting_code", url: "https://claude.com/cai/oauth/authorize?code=true&client_id=mock", message: null };
        }
      }, 800);
      return clone(login);
    },
    claudeLoginCode: async (code) => {
      if (!/^[\x21-\x7e]+$/.test(code.trim())) throw new ApiError("that doesn't look like a sign-in code", 400);
      if (login.state !== "awaiting_code") throw new ApiError("no Claude sign-in is waiting for a code", 409);
      login = { ...login, state: "verifying" };
      setTimeout(() => {
        login = { state: "done", url: null, message: "Connected your Claude subscription" };
        claude = { configured: true, source: "Claude subscription", kind: "CLAUDE_CODE_OAUTH_TOKEN" };
      }, 1200);
      return clone(login);
    },
    claudeLoginCancel: async () => {
      login = { state: "idle", url: null, message: null };
      return clone(login);
    },
    openEvents: (id, since) => {
      const session = sessions.get(id);
      const socket = new MockSocket({
        open: (s) => {
          if (!session) {
            s.close();
            return;
          }
          session.attach(s, since);
        },
        message: (_s, data) => session?.command(data),
        close: (s) => session?.detach(s),
      });
      return socket as unknown as SocketLike;
    },
    openTerminal: (id) => mockTerminal(sessions.get(id)),

    plugins: () =>
      later(() => ({
        local_root: "/home/you/.local/share/colonizer/plugins",
        plugins: [
          {
            name: "ecc",
            description: "Harness-native ECC plugin for engineering teams - 68 agents, 286 skills, 94 legacy command shims",
            version: "2.2.1",
            source: "vendored" as const,
            shadows_vendored: false,
            skills: 286,
            agents: 68,
            commands: 94,
          },
          {
            name: "team-skills",
            description: "House style, release checklist and the incident runbook",
            version: "0.3.0",
            source: "local" as const,
            shadows_vendored: false,
            skills: 4,
            agents: 0,
            commands: 1,
          },
        ],
      })),
    providers: () => later(() => providers),
    saveProvider: async (id, body) => {
      await sleep(250);
      if (!/^[a-z0-9][a-z0-9-]{0,31}$/.test(id) || id === "anthropic") {
        throw new ApiError('provider ids are lowercase letters, digits and dashes, and can\'t be "anthropic"', 400);
      }
      if (!body.name.trim()) throw new ApiError("provider name must be 1-60 characters", 400);
      if (!/^https?:\/\/[^\s/]+/.test(body.base_url.trim())) throw new ApiError("base URL must be an http(s) URL like https://api.deepseek.com/anthropic", 400);
      for (const [key, min, max] of LIMIT_RANGES) {
        const value = body[key];
        if (value != null && (!Number.isInteger(value) || (value as number) < min || (value as number) > max)) {
          throw new ApiError(`${key} must be between ${min} and ${max}`, 400);
        }
      }
      const fallback = body.fallback_model?.trim() || null;
      if (fallback && (fallback.includes("/") || !/^[a-z0-9][a-z0-9.-]*$/.test(fallback))) {
        throw new ApiError("fallback_model must be an Anthropic model id or alias like sonnet", 400);
      }
      const existing = providers.find((p) => p.id === id);
      const has_key = body.api_key === undefined ? (existing?.has_key ?? false) : body.api_key.trim() !== "";
      const provider: ModelProvider = {
        id,
        name: body.name.trim(),
        base_url: body.base_url.trim().replace(/\/+$/, ""),
        auth: body.auth,
        wire: body.wire ?? existing?.wire ?? "anthropic",
        has_key,
        models: body.models.map((m) => m.trim()).filter(Boolean),
        preset: body.preset ?? existing?.preset ?? "custom",
        timeout_secs: body.timeout_secs ?? DEFAULT_LIMITS.timeout_secs,
        max_concurrent: body.max_concurrent ?? null,
        queue_timeout_secs: body.queue_timeout_secs ?? null,
        context_tokens: body.context_tokens ?? null,
        fallback_model: fallback,
        in_flight: existing?.in_flight ?? 0,
        queued: existing?.queued ?? 0,
      };
      if (existing) Object.assign(existing, provider);
      else providers.push(provider);
      return clone(provider);
    },
    deleteProvider: async (id) => {
      const index = providers.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such provider", 404);
      providers.splice(index, 1);
      return { ok: true };
    },
    providerHealth: async (id) => {
      const provider = providers.find((p) => p.id === id);
      if (!provider) throw new ApiError("no such provider", 404);
      // strix is a local server that is switched off; custom endpoints answer but have no /v1/models.
      await sleep(provider.id === "strix" ? 2200 : 700);
      const checked_at = now();
      if (provider.id === "strix") {
        return { reachable: false, status: null, latency_ms: null, models: [], error: "connect timed out after 5 s", checked_at };
      }
      if (provider.preset === "custom") {
        return { reachable: true, status: 404, latency_ms: 38, models: [], error: "GET /v1/models returned 404", checked_at };
      }
      return { reachable: true, status: 200, latency_ms: 42, models: provider.models.length ? provider.models : ["ds4-flash"], error: null, checked_at };
    },
    models: () =>
      later((): ModelOption[] => [
        ...ANTHROPIC_MODELS.map(([id, label]) => ({ id, label, provider: "anthropic" })),
        ...providers.flatMap((p) => p.models.map((model) => ({ id: `${p.id}/${model}`, label: `${model} · ${p.name}`, provider: p.id }))),
      ]),

    orgs: () =>
      later((): OrgInfo[] => {
        const names = new Set([
          ...REPOS.map((r) => r.full_name.split("/")[0]),
          ...[...sessions.values()].map((s) => s.session.repo.split("/")[0]),
          ...Object.keys(orgSettings),
        ]);
        return [...names].sort().map((org) => {
          const colonies = [...sessions.values()].filter((s) => s.session.repo.split("/")[0] === org);
          return {
            org,
            colonies: { live: colonies.filter((s) => isLive(s.session.status)).length, total: colonies.length },
            pending_memory: proposals.filter((p) => orgOfKey(p) === org).length,
            settings: orgSettings[org] ?? {},
          };
        });
      }),
    saveOrg: async (org, settings) => {
      await sleep(250);
      orgSettings[org] = clone(settings);
      return clone({ org, settings });
    },

    memory: (scope, key) =>
      later(() => ({
        scope,
        key,
        notes: notes.filter((n) => n.scope === scope && n.key === key).sort((a, b) => b.created_at.localeCompare(a.created_at)),
        proposals: proposals.filter((p) => p.scope === scope && p.key === key),
      })),
    memoryProposals: () => later(() => [...proposals].sort((a, b) => b.created_at.localeCompare(a.created_at))),
    approveProposal: async (id, edits) => {
      await sleep(250);
      const index = proposals.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such proposal", 404);
      const [proposal] = proposals.splice(index, 1);
      const { status: _status, ...rest } = proposal;
      const note: MemoryNote = {
        ...rest,
        id: `note-${Math.random().toString(16).slice(2, 8)}`,
        title: edits?.title?.trim() || proposal.title,
        content: edits?.content?.trim() || proposal.content,
        created_at: now(),
      };
      notes.push(note);
      return clone(note);
    },
    rejectProposal: async (id) => {
      await sleep(200);
      const index = proposals.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such proposal", 404);
      proposals.splice(index, 1);
      return { ok: true };
    },
    createNote: async (body) => {
      await sleep(250);
      if (!body.title.trim() || !body.content.trim()) throw new ApiError("a note needs a title and content", 400);
      if (body.scope !== "global" && !body.key) throw new ApiError("org and repo notes need a key", 400);
      const note: MemoryNote = {
        id: `note-${Math.random().toString(16).slice(2, 8)}`,
        scope: body.scope,
        key: body.scope === "global" ? "" : body.key,
        title: body.title.trim(),
        content: body.content.trim(),
        tags: [],
        created_at: now(),
        source: { user: true },
      };
      notes.push(note);
      return clone(note);
    },
    deleteNote: async ({ id, scope, key }) => {
      await sleep(200);
      const index = notes.findIndex((n) => n.id === id && n.scope === scope && n.key === key);
      if (index < 0) throw new ApiError("no such note", 404);
      notes.splice(index, 1);
      return { ok: true };
    },
  };
}
