// A launch refused as a duplicate (issue #832): the 409's `duplicate` is read off the error and shown
// with the holder linked. Rendered through react-dom/server like the rest of the cockpit's tests.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApiError, duplicateHolder } from "../api";
import type { DuplicateHolder } from "../types";
import { DuplicateNotice, describeHolder } from "./DuplicateNotice";

const holder = (overrides: Partial<DuplicateHolder> = {}): DuplicateHolder => ({
  kind: "issue",
  colony: "ab12cd34",
  host: null,
  status: "pr_opened",
  pr_url: "https://github.com/acme/app/pull/9",
  issue: 7,
  what: "#7",
  queueable: true,
  ...overrides,
});

describe("duplicateHolder", () => {
  it("reads the holder off a 409 body and nothing else", () => {
    const body = { error: "colony ab12cd34 is already on #7", duplicate: holder() };
    expect(duplicateHolder(new ApiError("held", 409, body))).toEqual(holder());
    expect(duplicateHolder(new ApiError("held", 409, { error: "an older mothership" }))).toBeNull();
    expect(duplicateHolder(new ApiError("epic", 409, null))).toBeNull();
    expect(duplicateHolder(new ApiError("bad", 400, body))).toBeNull();
    expect(duplicateHolder(new Error("network"))).toBeNull();
  });
});

describe("DuplicateNotice", () => {
  it("links the holding colony and its pull request and names both ways past it", () => {
    const out = renderToStaticMarkup(<DuplicateNotice holder={holder()} onOpen={() => {}} />);
    expect(out).toContain("#7</span> is already being done by");
    expect(out).toContain('<button type="button"');
    expect(out).toContain(">ab12cd34</button>");
    expect(out).toContain("pr opened");
    expect(out).toContain('href="https://github.com/acme/app/pull/9"');
    expect(out).toContain("Allow duplicate");
    expect(out).toContain("Wait behind the holder");
  });

  it("names another mothership's claim by host, without a link to a colony it cannot open", () => {
    const remote = holder({ kind: "remote_claim", host: "archlinux (f88)", status: null, queueable: false });
    const out = renderToStaticMarkup(<DuplicateNotice holder={remote} onOpen={() => {}} />);
    expect(out).not.toContain("<button");
    expect(out).toContain("ab12cd34</span> on archlinux (f88)");
    expect(out).toContain("Allow duplicate");
    expect(out).not.toContain("Wait behind the holder");
  });

  it("offers no wait for a supply-chain hold, and says it in one sentence for a toast", () => {
    const supply = holder({ kind: "supply_chain", what: "hyper / rustsec-1", pr_url: null, status: "running", issue: null, queueable: false });
    expect(renderToStaticMarkup(<DuplicateNotice holder={supply} />)).not.toContain("Wait behind the holder");
    expect(describeHolder(supply)).toBe("hyper / rustsec-1 is already being done by colony ab12cd34 (it is running).");
  });
});
