// What Colonizer is built with, and the two words that keep the list honest (issue #944): a product
// in use today reads `live`, one named for later reads `planned`, and the tone says the same thing
// the word does — a planned entry is never dressed as a live one. Rendered to static markup — the
// test environment has no DOM — and the pane takes its data as a plain prop, so no context provider
// stands between the fixture and the markup.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { BuiltWith, BuiltWithUse } from "../../types";
import { BuiltWithPane } from "./BuiltWithPane";

const use = (over: Partial<BuiltWithUse> = {}): BuiltWithUse => ({
  id: "FZ-004",
  kind: "factory-zero",
  name: "Cratefield",
  note: "The fleet waitlist Worker runs on the Cratefield harness.",
  phrase: "Built with",
  role: "framework",
  status: "live",
  url: "https://cratefield.com/",
  ...over,
});

const builtWith = (over: Partial<BuiltWith> = {}): BuiltWith => ({
  registry: "https://factory0.ventures/stack.json",
  venture: "FZ-006",
  venture_name: "Colonizer",
  venture_page: "https://factory0.ventures/ventures/colonizer/",
  retrieved: "2026-10-05",
  uses: [use()],
  ...over,
});

/** One entry's markup on its own, so a tone assertion is about that entry and not its neighbours. */
const entryMarkup = (entry: BuiltWithUse) => {
  const out = renderToStaticMarkup(<BuiltWithPane builtWith={builtWith({ uses: [entry] })} />);
  // The entry sits between the "Live today" flow's closing and the provenance line; the status
  // badge is the only `rounded-full` in the pane, so slice up to it.
  return out;
};

describe("BuiltWithPane", () => {
  it("says so before the list arrives", () => {
    const out = renderToStaticMarkup(<BuiltWithPane builtWith={null} />);
    expect(out).toContain("Loading…");
  });

  it("lists every entry: its phrase, the product as a link, and what it is for", () => {
    const out = renderToStaticMarkup(
      <BuiltWithPane
        builtWith={builtWith({
          uses: [
            use(),
            use({ id: "FZ-013", name: "Owlpost", phrase: "Email by", role: "email", status: "planned", url: "https://owlpost.to/", note: "Waitlist confirmation mail." }),
            use({ id: "cloudflare", kind: "third-party", name: "Cloudflare", phrase: "Hosted on", role: "hosting", status: "live", url: "https://www.cloudflare.com", note: "The site and the waitlist Worker." }),
          ],
        })}
      />,
    );
    expect(out).toContain("Built with");
    expect(out).toContain("Email by");
    expect(out).toContain("Hosted on");
    expect(out).toContain("Cratefield");
    expect(out).toContain("Owlpost");
    expect(out).toContain("Cloudflare");
    expect(out).toContain("The fleet waitlist Worker runs on the Cratefield harness.");
    expect(out).toContain("Waitlist confirmation mail.");
    expect(out).toContain("The site and the waitlist Worker.");
    // Every product name links to the page that backs the claim, plus the two provenance links.
    expect(out).toContain('href="https://cratefield.com/"');
    expect(out).toContain('href="https://owlpost.to/"');
    expect(out).toContain('href="https://www.cloudflare.com"');
    expect(out.match(/rel="noreferrer"/g)?.length).toBe(5);
  });

  it("spells the status out in words, and a planned entry never wears the live tone", () => {
    const live = entryMarkup(use({ status: "live" }));
    expect(live).toContain("live");
    expect(live).toContain("bg-ok-soft");
    expect(live).not.toContain("bg-panel-2");

    const planned = entryMarkup(use({ status: "planned" }));
    expect(planned).toContain("planned");
    expect(planned).not.toContain("bg-ok-soft");
    // The neutral tone, not the ok one: a plan is not a fault and not a claim of use.
    expect(planned).toContain("bg-panel-2");
    expect(planned).not.toContain("bg-err-soft");
    expect(planned).not.toContain("bg-warn-soft");
  });

  it("counts each word, so a list of eight does not read as eight live things", () => {
    const out = renderToStaticMarkup(
      <BuiltWithPane
        builtWith={builtWith({
          uses: [use({ status: "live" }), use({ id: "a", status: "planned" }), use({ id: "b", status: "planned" })],
        })}
      />,
    );
    expect(out.match(/>live</g)?.length).toBe(1);
    expect(out.match(/>planned</g)?.length).toBe(2);
    expect(out.match(/bg-ok-soft/g)?.length).toBe(1);
  });

  it("carries the provenance: both pages, and the day the copy was taken", () => {
    const out = renderToStaticMarkup(<BuiltWithPane builtWith={builtWith()} />);
    expect(out).toContain('href="https://factory0.ventures/ventures/colonizer/"');
    expect(out).toContain('href="https://factory0.ventures/stack.json"');
    expect(out).toContain("venture page");
    expect(out).toContain("stack registry");
    expect(out).toContain("2026-10-05");
    expect(out).toContain("Colonizer");
  });

  it("does not name a day it never read one", () => {
    const out = renderToStaticMarkup(<BuiltWithPane builtWith={builtWith({ retrieved: null })} />);
    expect(out).toContain('href="https://factory0.ventures/stack.json"');
    expect(out).not.toContain("copied on");
  });

  it("says so when nothing is published, rather than drawing an empty list", () => {
    const out = renderToStaticMarkup(<BuiltWithPane builtWith={builtWith({ uses: [] })} />);
    expect(out).toContain("Nothing published");
  });
});
