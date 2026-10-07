// The OS mark next to the Setup machine row (issue #208): a drawn glyph for the vendors we drew
// one for, the vendor's short name as a text chip for the rest, and nothing when the motherboard
// sent no os — never a broken image. Rendered to static markup: the test environment has no DOM,
// and the serialized markup is exactly the output the browser paints.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { OsLogo } from "./OsLogo";
import type { OsInfo } from "../types";

const apple: OsInfo = { vendor: "apple", name: "macOS", version: "14.5", id: null };
const ubuntu: OsInfo = { vendor: "ubuntu", name: "Ubuntu", version: "24.04", id: "ubuntu" };

const markup = (os?: OsInfo | null) => renderToStaticMarkup(<OsLogo os={os} />);

describe("OsLogo", () => {
  it("draws an inline svg for a vendor we drew a glyph for", () => {
    expect(markup(apple)).toContain("<svg");
    expect(markup(apple)).toContain('viewBox="0 0 24 24"');
    // Ubuntu's glyph is the ring: a stroked circle on the shared 24 px grid.
    expect(markup(ubuntu)).toContain("<svg");
    expect(markup(ubuntu)).toContain("<circle");
  });

  it("draws the neutral desktop mark for the unknown vendor", () => {
    const unknown: OsInfo = { vendor: "unknown", name: "Something", version: null, id: null };
    const out = markup(unknown);
    expect(out).toContain("<svg");
    // The mark is the generic monitor: a rounded screen rect plus its stand.
    expect(out).toContain("<rect");
  });

  it("renders an undrawn vendor's short name as text rather than a broken box", () => {
    const rhel: OsInfo = { vendor: "rhel", name: "Red Hat Enterprise Linux", version: "9.4", id: "rhel" };
    const omarchy: OsInfo = { vendor: "omarchy", name: "Omarchy", version: "1.0", id: null };
    const neverHeard = { vendor: "kittenos", name: "Kitten OS", version: "1", id: null };
    for (const os of [rhel, omarchy, neverHeard]) {
      const out = markup(os);
      expect(out).not.toContain("<svg");
      expect(out).toContain(os.vendor);
    }
  });

  it("renders nothing when os is missing or null — never an empty or broken box", () => {
    expect(markup()).toBe("");
    expect(markup(null)).toBe("");
  });

  it("carries the full name and version in the title, for glyphed and text-chip vendors alike", () => {
    expect(markup(apple)).toContain('title="macOS 14.5"');
    const rhel: OsInfo = { vendor: "rhel", name: "Red Hat Enterprise Linux", version: "9.4", id: "rhel" };
    expect(markup(rhel)).toContain('title="Red Hat Enterprise Linux 9.4"');
  });
});