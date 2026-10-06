// The "Built with" list the mock install answers with (issue #944): the same eight entries the
// mothership vendors from `built-with.json`, so `?mock=1` shows the section as a live mothership
// would. The copy is verbatim, apostrophes included — the demo build is the one place a reader sees
// the list without a mothership, so it must match what a real one serves.
// No shared state: the list is a fact about the install, not something a call can change.
import { clone, sleep } from "../../mockShared";
import type { BuiltWithApi } from "./api";
import type { BuiltWith } from "./types";

const BUILT_WITH: BuiltWith = {
  registry: "https://factory0.ventures/stack.json",
  venture: "FZ-006",
  venture_name: "Colonizer",
  venture_page: "https://factory0.ventures/ventures/colonizer/",
  retrieved: "2026-10-05",
  uses: [
    {
      id: "FZ-004",
      kind: "factory-zero",
      name: "Cratefield",
      note: "The fleet waitlist Worker at api.colonizer.dev runs on the Cratefield harness, and the app uses its telemetry module.",
      phrase: "Built with",
      role: "framework",
      status: "live",
      url: "https://cratefield.com/",
    },
    {
      id: "FZ-013",
      kind: "factory-zero",
      name: "Owlpost",
      note: "Waitlist confirmation mail. Double opt-in is off today, so no mail is sent yet.",
      phrase: "Email by",
      role: "email",
      status: "planned",
      url: "https://owlpost.to/",
    },
    {
      id: "FZ-008",
      kind: "factory-zero",
      name: "SupportGenius",
      note: "Errors and bug reports become deduplicated GitHub issues, through the Cratefield error reporter.",
      phrase: "Bug reports to",
      role: "bug-reports",
      status: "planned",
      url: "https://supportgeni.us/",
    },
    {
      id: "polar",
      kind: "third-party",
      name: "Polar",
      note: "Paid plans through Polar as Merchant of Record, via the Cratefield Payments port. Nothing is on sale yet.",
      phrase: "Payments by",
      role: "payments",
      status: "planned",
      url: "https://polar.sh",
    },
    {
      id: "FZ-009",
      kind: "factory-zero",
      name: "promptdecode",
      note: "Colony output is screened before a pull request opens by the screen module’s promptdecode provider, a built-in decoder for promptdecode’s three code-point classes.",
      phrase: "Screens with the method of",
      role: "screening-engine",
      status: "live",
      url: "https://promptdeco.de/",
    },
    {
      id: "FZ-016",
      kind: "factory-zero",
      name: "Sealbin",
      note: "Optional sealed handoffs between colonies on different machines.",
      phrase: "Handoffs sealed by",
      role: "secrets-handoff",
      status: "planned",
      url: "https://sealb.in/",
    },
    {
      id: "FZ-012",
      kind: "factory-zero",
      name: "Keep Shipping",
      note: "Every venture site and Worker deployed from the Keep Shipping console.",
      phrase: "Deploys by",
      role: "deploys",
      status: "planned",
      url: "https://keepshipping.run/",
    },
    {
      id: "cloudflare",
      kind: "third-party",
      name: "Cloudflare",
      note: "The site and the waitlist Worker.",
      phrase: "Hosted on",
      role: "hosting",
      status: "live",
      url: "https://www.cloudflare.com",
    },
  ],
};

export function builtWithMock(): BuiltWithApi {
  return {
    builtWith: async () => {
      await sleep(150);
      return clone(BUILT_WITH);
    },
  };
}
