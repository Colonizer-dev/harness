import { describe, expect, it } from "vitest";
import { compact, monacoLanguage, shares, suggestBranch, sumLoc, unifiedDiff } from "./code";

describe("Code page helpers", () => {
  it("sums lines across repositories, biggest language first", () => {
    const loc = (rows: [string, number][]) => ({ ref: "main", sha: "a", total: rows.reduce((t, [, c]) => t + c, 0), by_language: rows.map(([name, code]) => ({ name, code, files: 1, blank: 0 })) });
    const s = sumLoc([loc([["Rust", 100], ["TSX", 50]]), null, loc([["TSX", 80]])]);
    expect(s.total).toBe(230);
    expect(s.by_language.map((l) => [l.name, l.code])).toEqual([["TSX", 130], ["Rust", 100]]);
    expect(shares(s.by_language).map((r) => r.percent)).toEqual([56.5, 43.5]);
    expect(shares([])).toEqual([]);
  });

  it("formats counts and picks Monaco languages", () => {
    expect([compact(950), compact(1234), compact(48210), compact(2_500_000)]).toEqual(["950", "1.2k", "48k", "2.5M"]);
    expect(monacoLanguage("src/main.rs")).toBe("rust");
    expect(monacoLanguage("web/App.tsx")).toBe("typescript");
    expect(monacoLanguage("Dockerfile")).toBe("dockerfile");
    expect(monacoLanguage("LICENSE")).toBe("plaintext");
  });

  it("suggests a branch and diffs line by line", () => {
    expect(suggestBranch(["src/Gateway Handler.rs"], () => 0)).toBe("edit/gateway-handler-0000");
    const d = unifiedDiff("a.txt", "one\ntwo\nthree", "one\n2\nthree");
    expect(d.split("\n")).toEqual(["--- a/a.txt", "+++ b/a.txt", " one", "+2", "-two", " three"]);
  });
});
