// What each settings page is for, in a few words and a picture: the icon on its nav chip, the
// hero card at the top of the page (icon tile, one-line blurb, a small flow of icon nodes, and
// a few stat chips), and which module fields are technical enough to fold under "Advanced".
import type { ReactNode } from "react";
import type { SchemaField } from "../types";
import { cx } from "./ui";

/** Stroke icons on the 24px grid, drawn like icons.tsx so the two sets sit together. */
const PATHS: Record<string, ReactNode> = {
  check: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="m8.5 12.2 2.4 2.4 4.6-5" />
    </>
  ),
  plug: <path d="M9 3v5M15 3v5M6.5 8h11v3a5.5 5.5 0 0 1-11 0zM12 16.5V21" />,
  spark: (
    <path d="M12 3.5 13.9 9l5.6 1.9-5.6 1.9L12 18.5l-1.9-5.7-5.6-1.9L10.1 9z" />
  ),
  cpu: (
    <>
      <rect x="6" y="6" width="12" height="12" rx="2" />
      <rect x="9.5" y="9.5" width="5" height="5" rx="1" />
      <path d="M9.5 3v3M14.5 3v3M9.5 18v3M14.5 18v3M3 9.5h3M3 14.5h3M18 9.5h3M18 14.5h3" />
    </>
  ),
  globe: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17M12 3.5c2.4 2.4 3.5 5.2 3.5 8.5s-1.1 6.1-3.5 8.5c-2.4-2.4-3.5-5.2-3.5-8.5S9.6 5.9 12 3.5z" />
    </>
  ),
  download: <path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M5 19.5h14" />,
  chart: <path d="M4.5 19.5h15M7.5 16v-4M12 16V8M16.5 16v-6.5" />,
  bell: (
    <>
      <path d="M6.5 16.5V11a5.5 5.5 0 0 1 11 0v5.5l1.5 1.5H5z" />
      <path d="M10 20.5h4" />
    </>
  ),
  branch: (
    <>
      <circle cx="7" cy="5.5" r="2" />
      <circle cx="7" cy="18.5" r="2" />
      <circle cx="17" cy="8" r="2" />
      <path d="M7 7.5v9M17 10c0 4-4.5 3.5-8.6 7" />
    </>
  ),
  box: (
    <>
      <path d="m12 3.5 8 4.5v8l-8 4.5-8-4.5V8z" />
      <path d="m4 8 8 4.5L20 8M12 12.5v8" />
    </>
  ),
  mesh: (
    <>
      <circle cx="12" cy="5" r="2" />
      <circle cx="5" cy="18" r="2" />
      <circle cx="19" cy="18" r="2" />
      <path d="M11 6.8 6 16.2M13 6.8l5 9.4M7 18h10" />
    </>
  ),
  ant: (
    <>
      <circle cx="12" cy="6.5" r="2" />
      <ellipse cx="12" cy="12" rx="2.3" ry="2.6" />
      <ellipse cx="12" cy="18" rx="2.8" ry="3" />
      <path d="M9.8 11 6 9M14.2 11 18 9M9.5 13.5 5.5 15M14.5 13.5l4 1.5M10.5 4.8 9 3M13.5 4.8 15 3" />
    </>
  ),
  panels: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
      <path d="M3.5 9h17M10 9v10.5" />
    </>
  ),
  pr: (
    <>
      <circle cx="6.5" cy="5.5" r="2" />
      <circle cx="6.5" cy="18.5" r="2" />
      <circle cx="17.5" cy="18.5" r="2" />
      <path d="M6.5 7.5v9M17.5 16.5V10a3 3 0 0 0-3-3H11M13 5 11 7l2 2" />
    </>
  ),
  memory: (
    <>
      <path d="m12 3.5 8.5 4.5-8.5 4.5L3.5 8z" />
      <path d="m3.5 12 8.5 4.5 8.5-4.5M3.5 16l8.5 4.5 8.5-4.5" />
    </>
  ),
  eye: (
    <>
      <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" />
      <circle cx="12" cy="12" r="2.8" />
    </>
  ),
  shield: (
    <>
      <path d="M12 3.5 19 6v5.5c0 4.3-3 7.6-7 9-4-1.4-7-4.7-7-9V6z" />
      <path d="m9 12 2.2 2.2L15.5 10" />
    </>
  ),
  flame: (
    <path d="M12 21c-3.6 0-6-2.4-6-5.6 0-3.6 3-5.4 3.8-9.4 2.4 1.6 3.4 3.6 3.4 5.4 1-.6 1.6-1.8 1.8-3 1.8 1.6 3 4 3 6.6C18 18.6 15.6 21 12 21z" />
  ),
  mic: (
    <>
      <rect x="9" y="3.5" width="6" height="11" rx="3" />
      <path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v2.5" />
    </>
  ),
  org: (
    <>
      <rect x="4" y="3.5" width="9" height="17" rx="1.5" />
      <path d="M13 9h5.5a1.5 1.5 0 0 1 1.5 1.5v10H13M7 7.5h3M7 11h3M7 14.5h3" />
    </>
  ),
  lock: (
    <>
      <rect x="5.5" y="10.5" width="13" height="9.5" rx="2" />
      <path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5" />
    </>
  ),
  mothership: (
    <>
      <path d="M12 3 19.8 7.5v9L12 21l-7.8-4.5v-9z" />
      <circle cx="12" cy="12" r="2.4" />
    </>
  ),
  github: (
    <path d="M9 19c-4 1.3-4-2-5.5-2.5M14.5 21v-3.2c0-1 .1-1.5-.5-2 2.8-.3 5.5-1.4 5.5-6a4.7 4.7 0 0 0-1.3-3.3 4.4 4.4 0 0 0-.1-3.3s-1-.3-3.4 1.3a11.6 11.6 0 0 0-6.2 0C6.1 2.9 5.1 3.2 5.1 3.2A4.4 4.4 0 0 0 5 6.5a4.7 4.7 0 0 0-1.3 3.3c0 4.6 2.7 5.7 5.5 6-.6.5-.6 1.2-.5 2V21" />
  ),
  user: (
    <>
      <circle cx="12" cy="8" r="3.5" />
      <path d="M5 20c.8-3.6 3.5-5.5 7-5.5s6.2 1.9 7 5.5" />
    </>
  ),
  code: <path d="m8.5 7-5 5 5 5M15.5 7l5 5-5 5M13.5 4.5l-3 15" />,
  issue: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <circle cx="12" cy="12" r="1.6" />
    </>
  ),
  chat: (
    <path d="M20.5 12a8 8 0 0 1-11.6 7.1L3.5 20.5l1.4-5A8 8 0 1 1 20.5 12z" />
  ),
  sliders: (
    <path d="M5 20v-6M5 10V4M12 20v-8M12 8V4M19 20v-4M19 12V4M3 14h4M10 8h4M17 16h4" />
  ),
  text: <path d="M4.5 6.5h15M4.5 12h15M4.5 17.5h9" />,
  queue: <path d="M4.5 6.5h15M4.5 12h15M4.5 17.5h15" />,
  filter: <path d="M4 5h16l-6.2 7.4V19l-3.6-1.8v-4.8z" />,
};

/** A provider as its mark and its name, e.g. the GitHub mark beside "GitHub". */
export function ModuleProviderMark({ id, name }: { id: string; name: string }) {
  return (
    <span className="inline-flex min-w-0 items-center gap-2 text-[13.5px] font-medium text-text">
      <span className="grid size-6 shrink-0 place-items-center rounded-md border border-border bg-panel-2">
        <GuideIcon name={id in PATHS ? id : "plug"} size={14} />
      </span>
      <span className="truncate">{name}</span>
    </span>
  );
}

export function GuideIcon({
  name,
  size = 16,
  className,
}: {
  name: string;
  size?: number;
  className?: string;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className={cx("shrink-0", className)}
    >
      {PATHS[name] ?? PATHS.sliders}
    </svg>
  );
}

export type FlowChip = { text: string; kind: "in" | "out" | "note" };
export type FlowNode = {
  icon: string;
  label: string;
  lock?: boolean;
  /** A live figure under the label, e.g. "3 running". */
  metric?: string;
  /** Lights the node: the step where things are happening now. */
  active?: boolean;
  /** Label chips inside the node, e.g. a filter's include / exclude lists. */
  chips?: FlowChip[];
};
export interface Guide {
  icon: string;
  blurb: string;
  flow: FlowNode[];
  /** Arrows run both ways (a network, a conversation) rather than one way. */
  both?: boolean;
}

const GUIDES: Record<string, Guide> = {
  setup: {
    icon: "check",
    blurb: "A short checklist to get your first colony running.",
    flow: [
      { icon: "plug", label: "Connect" },
      { icon: "box", label: "Stack" },
      { icon: "ant", label: "Launch" },
    ],
  },
  connections: {
    icon: "plug",
    blurb: "Links GitHub and Claude, so colonies can read code and think.",
    flow: [
      { icon: "github", label: "GitHub" },
      { icon: "mothership", label: "Mothership" },
      { icon: "spark", label: "Claude" },
    ],
    both: true,
  },
  providers: {
    icon: "spark",
    blurb: "The AI models your colonies are allowed to call.",
    flow: [
      { icon: "ant", label: "Colony" },
      { icon: "mothership", label: "Gateway" },
      { icon: "spark", label: "Models" },
    ],
  },
  runtime: {
    icon: "cpu",
    blurb: "What this machine has on hand to run colonies.",
    flow: [
      { icon: "cpu", label: "This machine" },
      { icon: "box", label: "microVMs" },
      { icon: "ant", label: "Colonies" },
    ],
  },
  "live-map": {
    icon: "globe",
    blurb: "Shows this mothership as one anonymous dot on colonizer.dev.",
    flow: [
      { icon: "mothership", label: "Mothership" },
      { icon: "lock", label: "Anonymous" },
      { icon: "globe", label: "Live map" },
    ],
  },
  remote: {
    icon: "globe",
    blurb: "Opens this cockpit from your phone or another computer, through the Colonizer relay.",
    flow: [
      { icon: "user", label: "Your phone" },
      { icon: "lock", label: "Paired sign-in" },
      { icon: "mothership", label: "This cockpit" },
    ],
    both: true,
  },
  updates: {
    icon: "download",
    blurb: "Keeps Colonizer current with the latest release.",
    flow: [
      { icon: "github", label: "Releases" },
      { icon: "download", label: "Download" },
      { icon: "mothership", label: "Installed" },
    ],
  },
  usage: {
    icon: "chart",
    blurb: "An anonymous usage summary you can read before anything is sent.",
    flow: [
      { icon: "mothership", label: "Mothership" },
      { icon: "chart", label: "Summary" },
      { icon: "eye", label: "You review" },
    ],
  },
  desktop: {
    icon: "mothership",
    blurb: "The cockpit in its own window, and the mothership already running when you log in.",
    flow: [
      { icon: "user", label: "Login" },
      { icon: "mothership", label: "Mothership" },
      { icon: "ant", label: "Colonies" },
    ],
  },
  notifications: {
    icon: "bell",
    blurb: "Tells you when a colony needs an answer.",
    flow: [
      { icon: "ant", label: "Colony" },
      { icon: "chat", label: "Question" },
      { icon: "bell", label: "You" },
    ],
  },
  source: {
    icon: "issue",
    blurb: "Where colonies pick up the work to do: open issues, narrowed by your labels.",
    flow: [
      { icon: "github", label: "Issues" },
      { icon: "filter", label: "Filter" },
      { icon: "queue", label: "Queue" },
      { icon: "ant", label: "Colonies" },
      { icon: "pr", label: "Pull requests" },
    ],
  },
  sandbox: {
    icon: "box",
    blurb: "Runs each colony in its own sealed microVM.",
    flow: [
      { icon: "mothership", label: "Mothership" },
      { icon: "box", label: "microVM", lock: true },
      { icon: "ant", label: "Agent" },
    ],
  },
  mesh: {
    icon: "mesh",
    blurb:
      "Keeps colonies talking to the mothership over a private, encrypted network.",
    flow: [
      { icon: "mothership", label: "Mothership" },
      { icon: "lock", label: "Encrypted" },
      { icon: "ant", label: "Colonies" },
    ],
    both: true,
  },
  agent: {
    icon: "ant",
    blurb: "The AI coder that does the work inside each colony.",
    flow: [
      { icon: "issue", label: "Issue" },
      { icon: "ant", label: "Agent" },
      { icon: "code", label: "Code" },
    ],
  },
  interfaces: {
    icon: "panels",
    blurb: "The panels you see when you open a colony.",
    flow: [
      { icon: "ant", label: "Colony" },
      { icon: "panels", label: "Panels" },
      { icon: "user", label: "You" },
    ],
  },
  publish: {
    icon: "pr",
    blurb: "Turns finished work into a pull request.",
    flow: [
      { icon: "ant", label: "Colony" },
      { icon: "branch", label: "Branch" },
      { icon: "pr", label: "Pull request" },
    ],
  },
  memory: {
    icon: "memory",
    blurb: "Shared notes that colonies learn from and add to.",
    flow: [
      { icon: "ant", label: "Colonies" },
      { icon: "memory", label: "Notes" },
      { icon: "ant", label: "Next colony" },
    ],
  },
  watchdog: {
    icon: "eye",
    blurb: "Spots stuck colonies and nudges them back to work.",
    flow: [
      { icon: "ant", label: "Colony" },
      { icon: "eye", label: "Watchdog" },
      { icon: "bell", label: "Nudge" },
    ],
  },
  autonomy: {
    icon: "shield",
    blurb: "Answers colony questions for you while you are away.",
    flow: [
      { icon: "chat", label: "Question" },
      { icon: "shield", label: "Autopilot" },
      { icon: "ant", label: "Colony" },
    ],
  },
  burn_down: {
    icon: "flame",
    blurb: "Spends leftover weekly tokens before the plan resets.",
    flow: [
      { icon: "chart", label: "Weekly plan" },
      { icon: "flame", label: "Burn-down" },
      { icon: "ant", label: "Colonies" },
    ],
  },
  screen: {
    icon: "shield",
    blurb: "Screens what a colony is about to publish for hidden code points.",
    flow: [
      { icon: "ant", label: "Colony" },
      { icon: "branch", label: "Diff" },
      { icon: "shield", label: "Screen" },
    ],
  },
  voice: {
    icon: "mic",
    blurb: "Turns what you say into text in the composer.",
    flow: [
      { icon: "mic", label: "Microphone" },
      { icon: "text", label: "Speech to text" },
      { icon: "chat", label: "Composer" },
    ],
  },
  org: {
    icon: "org",
    blurb: "Settings that apply only to this workspace's colonies.",
    flow: [
      { icon: "sliders", label: "Global" },
      { icon: "org", label: "Workspace" },
      { icon: "ant", label: "Its colonies" },
    ],
  },
};

/** The guide for a settings section id: a general section, `module:<kind>` or `org:<name>`. */
export function guideFor(id: string): Guide {
  if (id.startsWith("module:"))
    return (
      GUIDES[id.slice("module:".length)] ?? {
        icon: "sliders",
        blurb: "",
        flow: [],
      }
    );
  if (id.startsWith("org:")) return GUIDES.org;
  return GUIDES[id] ?? { icon: "sliders", blurb: "", flow: [] };
}

export type HeroStat = {
  label: string;
  value: string;
  tone?: "ok" | "warn" | "err";
};

/** The card at the top of a settings page: what it is for, as a picture, and where it stands. */
export function SectionHero({
  guide,
  stats = [],
  flow,
}: {
  guide: Guide;
  stats?: HeroStat[];
  /** Live nodes for this page's picture, in place of the guide's static ones. */
  flow?: FlowNode[];
}) {
  if (!guide.blurb) return null;
  const nodes = flow ?? guide.flow;
  return (
    <div className="mb-4 overflow-hidden rounded-xl border border-border bg-panel-2">
      <div className="flex items-center gap-3.5 p-4">
        <span className="grid size-12 shrink-0 place-items-center rounded-xl bg-accent-soft text-accent">
          <GuideIcon name={guide.icon} size={24} />
        </span>
        <p className="min-w-0 text-[14px] font-medium leading-snug text-text">
          {guide.blurb}
        </p>
      </div>
      {nodes.length > 0 && (
        <div className="hero-band border-t border-border px-4 pb-4 pt-5">
          <Flow nodes={nodes} both={guide.both} />
        </div>
      )}
      {stats.length > 0 && (
        <div className="flex flex-wrap gap-2 border-t border-border px-4 py-2.5">
          {stats.map((s) => (
            <span
              key={s.label}
              className="inline-flex items-center gap-1.5 rounded-full border border-border bg-panel px-2.5 py-1 text-[12px]"
            >
              {s.tone && (
                <span
                  aria-hidden="true"
                  className={cx(
                    "size-1.5 rounded-full",
                    s.tone === "ok"
                      ? "bg-ok"
                      : s.tone === "err"
                        ? "bg-err"
                        : "bg-warn",
                  )}
                />
              )}
              <span className="text-muted">{s.label}</span>
              <span className="font-medium tabular-nums text-text">
                {s.value}
              </span>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function Flow({ nodes, both }: { nodes: FlowNode[]; both?: boolean }) {
  return (
    <ol
      aria-label={nodes.map((n) => (n.metric ? `${n.label}: ${n.metric}` : n.label)).join(both ? " and " : " to ")}
      className="flex w-full items-start"
    >
      {nodes.map((node, i) => (
        <li key={node.label} className={cx("flex min-w-0 items-start", i > 0 && "flex-1")}>
          {i > 0 && <Link both={both} lit={Boolean(node.active || nodes[i - 1].active)} />}
          <FlowStep node={node} />
        </li>
      ))}
    </ol>
  );
}

/** A connector: a dashed line whose dashes travel toward the next step (both ways for a link). */
function Link({ both, lit }: { both?: boolean; lit: boolean }) {
  return (
    <svg
      viewBox="0 0 100 44"
      preserveAspectRatio="none"
      aria-hidden="true"
      className={cx("mt-0 h-11 min-w-6 flex-1", lit ? "text-accent" : "text-faint")}
    >
      <path d="M4 22H96" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2" vectorEffect="non-scaling-stroke" />
      <path
        d="M4 22H96"
        stroke="currentColor"
        strokeWidth="2"
        strokeDasharray="3 7"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
        className={both ? "flow-dash-both" : "flow-dash"}
      />
    </svg>
  );
}

function FlowStep({ node }: { node: FlowNode }) {
  return (
    <div className="flex w-[92px] shrink-0 flex-col items-center gap-1.5 text-center">
      <span
        className={cx(
          "relative grid size-11 place-items-center rounded-full border",
          node.active
            ? "border-accent bg-accent-soft text-accent shadow-[0_0_0_4px_var(--color-accent-soft)]"
            : "border-border-strong bg-panel text-muted",
        )}
      >
        <GuideIcon name={node.icon} size={19} />
        {node.lock && (
          <span className="absolute -bottom-0.5 -right-0.5 grid size-4 place-items-center rounded-full bg-accent text-on-accent">
            <GuideIcon name="lock" size={10} />
          </span>
        )}
      </span>
      <span className="text-[12px] font-medium leading-tight text-text">{node.label}</span>
      {node.metric && (
        <span className={cx("text-[11.5px] leading-tight tabular-nums", node.active ? "text-accent" : "text-muted")}>
          {node.metric}
        </span>
      )}
      {node.chips && node.chips.length > 0 && (
        <span className="flex max-w-[120px] flex-wrap justify-center gap-1">
          {node.chips.map((chip) => (
            <span
              key={`${chip.kind}:${chip.text}`}
              className={cx(
                "rounded-full border px-1.5 py-px text-[10.5px] leading-4",
                chip.kind === "in" && "border-ok/40 bg-ok/10 text-ok",
                chip.kind === "out" && "border-err/40 bg-err/10 text-err line-through decoration-err/60",
                chip.kind === "note" && "border-border text-muted",
              )}
            >
              {chip.text}
            </span>
          ))}
        </span>
      )}
    </div>
  );
}

// Ports, binds, timers, thresholds and paths: needed now and then, noise the rest of the time.
const ADVANCED =
  /port|bind|loopback|udp|tcp|socks|wireguard|_secs\b|seconds|timeout|interval|_ms\b|threshold|preserve|_dir\b|\bdir\b|directory|path|_url\b|\burl\b|endpoint|cidr|subnet|mtu|derp|stun|address/i;

/** True for a module field that belongs under "Advanced" rather than on the page itself. */
export function isAdvancedField(key: string, field: SchemaField): boolean {
  if (field.format === "plugin-dirs") return false;
  return ADVANCED.test(key) || ADVANCED.test(field.title ?? "");
}
