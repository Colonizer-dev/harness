import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { DEFAULT_DURATION, MAX_VISIBLE, ToastStack, splitMessage, stackOf, toToast, type ToastItem } from "./toasts";

describe("toasts", () => {
  it("keeps a short legacy message as the title", () => {
    expect(splitMessage("Saved acme workspace settings")).toEqual({ title: "Saved acme workspace settings" });
  });

  it("splits a long legacy message after its first sentence", () => {
    const { title, body } = splitMessage(
      "Drawing CHI-Ecosystem/chi-backend… A colony is reading the code and drawing it with archify (queued for a free slot).",
    );
    expect(title).toBe("Drawing CHI-Ecosystem/chi-backend…");
    expect(body).toMatch(/^A colony is reading the code/);
  });

  it("maps the legacy (message, tone) form onto a kind and a default duration", () => {
    const err = toToast(1, "Could not save", "error");
    expect(err.kind).toBe("error");
    expect(err.duration).toBe(DEFAULT_DURATION.error);
    const info = toToast(2, "Copied");
    expect(info.kind).toBe("info");
    expect(info.duration).toBe(5000);
  });

  it("stacks newest first and folds everything past four into +N more", () => {
    const list: ToastItem[] = Array.from({ length: 6 }, (_, i) => toToast(i, `toast ${i}`));
    const folded = stackOf(list, false);
    expect(folded.shown.map((t) => t.title)).toEqual(["toast 5", "toast 4", "toast 3", "toast 2"]);
    expect(folded.shown).toHaveLength(MAX_VISIBLE);
    expect(folded.hidden).toBe(2);
    expect(stackOf(list, true).shown).toHaveLength(6);
    const html = renderToStaticMarkup(<ToastStack toasts={list} onDismiss={() => {}} />);
    expect(html).toContain("+2 more");
    const cards = html.slice(html.indexOf("toast-card"));
    expect(cards.indexOf("toast 5")).toBeLessThan(cards.indexOf("toast 4"));
  });

  it("draws the rich form with its body, action and an assertive region for errors", () => {
    const html = renderToStaticMarkup(
      <ToastStack
        toasts={[toToast(1, { title: "Map queued", body: "Waiting for a free slot", kind: "success", action: { label: "Watch it work", onClick: () => {} } }), toToast(2, "Boom", "error")]}
        onDismiss={() => {}}
      />,
    );
    expect(html).toContain("Waiting for a free slot");
    expect(html).toContain("Watch it work");
    expect(html).toContain('role="alert"');
    expect(html).toMatch(/aria-live="assertive"[^>]*>Boom\./);
    expect(html).toContain("animation-duration:10000ms");
  });
});
