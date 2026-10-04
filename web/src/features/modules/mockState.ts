// The mock's per-call state slice for the modules feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { ModuleInfo } from "../../types";
import type { MockState } from "../../mockState";

export type ModulesMockState = {
    modules: ModuleInfo[];
};

export function installModulesMockState(ms: MockState): void {
  ms.modules = [
    { kind: "source", provider: "github", providers: [{ id: "github", name: "GitHub", description: "Issues from repositories you can access" }], enabled: true, settings: {}, schema: null },
    {
      kind: "sandbox",
      provider: "microsandbox",
      providers: [{ id: "microsandbox", name: "microsandbox", description: "Rootless libkrun microVMs" }],
      enabled: true,
      settings: { preset: "node", image: "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0", cpus: 4, memory: "8G", budget_usd: 0, host_disk: "0" },
      schema: {
        type: "object",
        properties: {
          // Same enum the mothership's schema declares; a preset is a stack you pick instead of
          // an image tag you type — 'auto' reads it off each repository at boot — and the pinned
          // digests below are the lock's own.
          preset: { type: "string", title: "Stack", enum: ["auto", "node", "python", "rust", "go", "custom"], default: "auto", description: "Picks the image and machine size for a colony. 'auto', the default, reads the stack off each repository when the colony boots. Choose 'custom' to set the fields below yourself; anything you set explicitly wins over the preset either way." },
          image: { type: "string", title: "Image", description: "glibc-based OCI image", default: "node:24-bookworm@sha256:6dac556d980b7f0e5498d08f08cee0ca67798b4ad6c23964a9214920e67758d0" },
          cpus: { type: "integer", title: "vCPUs", minimum: 1, maximum: 64, default: 4 },
          memory: { type: "string", title: "Memory", default: "8G" },
          max_parallel: { type: "integer", title: "Parallel colonies", minimum: 1, maximum: 16, default: 3 },
          repo_max_parallel: { type: "integer", title: "Parallel sessions per repository", minimum: 1, maximum: 32, default: 3 },
          budget_usd: { type: "number", title: "Budget per colony (USD)", minimum: 0, default: 0, description: "Dollars one colony may spend on models in total. 0, the default, means unlimited." },
          host_disk: { type: "string", title: "Host disk per colony", default: "0", format: "disk-size", description: "How much disk one colony may leave on the host, like 512M or 16G. 0, the default, means unlimited." },
          mask_paths: { type: "array", items: { type: "string" }, title: "Also mask", default: [], format: "mask-path-list", description: "Extra worktree-relative paths a colony never sees, on top of the built-ins (.env, .envrc, .npmrc, .netrc, .git-credentials, .pypirc). One path per entry, comma-separated; a trailing / means the whole directory, and matching is at any depth." },
          protect_paths: { type: "array", items: { type: "string" }, title: "Also protect", default: [], format: "path-list", description: "Extra worktree-relative paths a colony may read but never write, on top of the built-ins (.git/config, .git/hooks/, .gitmodules, .claude/, .codex/, .mcp.json, .devcontainer/, .vscode/, .idea/). One path per entry, comma-separated; a trailing / means the whole directory." },
          unmask_paths: { type: "array", items: { type: "string" }, title: "Unmask (opt out)", default: [], format: "path-list", description: "Paths a colony may see again: each is removed from both the masked and the protected sets, built-ins included. Every opt-out is logged on the colony at boot." },
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
      providers: [{ id: "claude-code", name: "Claude Code", description: "Claude Agent SDK runner", loop_tools: true }],
      enabled: true,
      settings: { model: "", subagent_model: "", background_model: "", plugins: "ecc", caveman: false, caveman_level: "full", headroom: false, rtk: false, jev_compaction: false },
      schema: {
        type: "object",
        properties: {
          model: { type: "string", title: "Orchestrator model", default: "", description: "Empty uses the Claude Code default" },
          subagent_model: { type: "string", title: "Subagent model", default: "", description: "Model id, alias, or <provider>/<model>; empty uses the orchestrator model" },
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
          jev_compaction: {
            type: "boolean",
            title: "Jev compaction",
            description: "Token savings: at compaction, stale tool calls are deleted by Jev score instead of the built-in summary. Sends conversation history to TypeSafe.",
            default: false,
          },
          jev_keep_threshold: { type: "number", title: "Jev keep threshold", minimum: 0, maximum: 1, default: 0.5 },
          jev_preserve_recent: { type: "integer", title: "Jev preserve recent", minimum: 0, maximum: 100, default: 6 },
        },
      },
    },
    {
      kind: "memory",
      provider: "files",
      providers: [
        { id: "files", name: "Shared memory", description: "Markdown notes per repository, org and globally, mounted read-only into colonies; agents propose new notes" },
        {
          id: "mem0",
          name: "mem0",
          description: "Approved notes stored in your mem0 project. Colonies read them exactly as they read files, most relevant to the task first; the key never enters a colony",
        },
      ],
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
      kind: "voice",
      provider: "browser",
      providers: [
        { id: "browser", name: "Browser", description: "The browser's own speech recognition. Nothing to connect; Chrome sends the audio to Google, Safari can recognise on the device" },
        { id: "openai", name: "OpenAI", description: "OpenAI's transcription API. Reuses an OpenAI model provider's key when you have one" },
        { id: "groq", name: "Groq", description: "Groq's hosted Whisper, fast and inexpensive. Reuses a Groq model provider's key when you have one" },
        { id: "deepgram", name: "Deepgram", description: "Deepgram's speech-to-text API" },
        { id: "elevenlabs", name: "ElevenLabs", description: "ElevenLabs Scribe speech-to-text" },
        { id: "openai_compatible", name: "OpenAI-compatible", description: "Any server speaking OpenAI's /audio/transcriptions: a local whisper server, LiteLLM, vLLM" },
      ],
      enabled: true,
      settings: {},
      schema: {
        type: "object",
        properties: {
          model: { type: "string", title: "Model", description: "gpt-4o-mini-transcribe (default), gpt-4o-transcribe or whisper-1", default: "gpt-4o-mini-transcribe" },
          language: { type: "string", title: "Language", description: "An ISO-639-1 code such as en or nl. Blank lets the service detect it", default: "" },
        },
      },
    },
    {
      kind: "autonomy",
      provider: "off",
      providers: [
        { id: "off", name: "Off", description: "Questions wait for you, however long that takes" },
        {
          id: "judge",
          name: "Judge model",
          description: "A model answers a colony's questions when nobody does, choosing only among the options the agent offered",
        },
      ],
      enabled: true,
      settings: {},
      schema: { type: "object", properties: {} },
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
      settings: { autopilot: true, draft: false, file_findings: true, autofix: false, automerge: false },
      schema: {
        type: "object",
        properties: {
          autopilot: { type: "boolean", title: "Open the PR automatically", default: true },
          draft: { type: "boolean", title: "Open PRs as drafts", default: false },
          file_findings: {
            type: "boolean",
            title: "File validated findings as issues",
            description: "When a colony notices a problem outside its task, its orchestrator has it confirmed and files it as an issue on the same repository, labelled colonizer-finding. Open issues with the same title are not filed again, and one colony files at most five.",
            default: true,
          },
          autofix: {
            type: "boolean",
            title: "Autofix validated findings",
            description: "When a colony files a validated finding, spawn a fix colony for it: a fresh colony whose pull request is reviewed by an independent session before anything merges. Can be switched off per colony at launch.",
            default: false,
          },
          automerge: {
            type: "boolean",
            title: "Merge fixes whose review passes",
            description: "Merge a fix colony's pull request when its independent review passes; requires autofix, since with no fix colonies there is nothing to merge. Can be switched off per colony at launch.",
            default: false,
          },
        },
      },
    },
    {
      kind: "burn_down",
      provider: "default",
      providers: [
        {
          id: "default",
          name: "Burn down",
          description: "Maxes out the weekly plan: near the weekly reset it launches bug-hunt colonies, paced across the window, until the allowance is down to whatever reserve you set",
        },
      ],
      enabled: true,
      settings: { reset_weekday: "Monday", reset_time: "00:00", lead_hours: 48, reserve_pct: 5, allowance_usd: 200, spend_usd_per_colony: 5, max_live: 2, repos: "acme/webshop, acme/design-system", instructions: "" },
      schema: {
        type: "object",
        properties: {
          reset_weekday: { type: "string", title: "Weekly reset day (UTC)", enum: ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"], default: "Monday", description: "The day of the week your plan's allowance resets" },
          reset_time: { type: "string", title: "Weekly reset time (UTC)", default: "00:00", description: "24-hour HH:MM in UTC" },
          lead_hours: { type: "number", title: "Hours before reset to start burning", minimum: 1, maximum: 168, default: 48, description: "How long before the weekly reset burn-down starts launching colonies" },
          reserve_pct: { type: "number", title: "Reserve (percent of allowance)", minimum: 0, maximum: 90, default: 5, description: "Percent of the weekly allowance left untouched when the reset lands" },
          allowance_usd: { type: "number", title: "Weekly allowance (USD, your estimate)", description: "Your estimate of the weekly plan allowance. Burn-down never launches without it — an invented number would be worse than no number" },
          spend_usd_per_colony: { type: "number", title: "Estimated spend per colony (USD)", minimum: 0.5, default: 5, description: "What one bug-hunt colony roughly burns, used to pace launches across the window" },
          max_live: { type: "integer", title: "Concurrent live burn-down colonies", minimum: 1, maximum: 8, default: 2, description: "Cap on how many burn-down colonies run at once" },
          repos: { type: "string", title: "Repositories to hunt in", default: "", description: "Comma-separated owner/repo list. Empty means burn-down is not configured and launches nothing" },
          instructions: { type: "string", title: "Custom hunt instructions", default: "", description: "When empty, a built-in bug-hunt prompt is used" },
        },
      },
    },
  ];

}
