import { describe, expect, it, vi } from "vitest";

import { copyShortcut, copyText, manualCopyHint, type CopyEnv } from "./copy";

/** A fake document: enough of the DOM surface execCopy touches, with the copy result scripted. */
function fakeDoc(execResult: boolean | (() => boolean)) {
  const area = {
    value: "",
    style: {} as Record<string, string>,
    setAttribute: vi.fn(),
    focus: vi.fn(),
    select: vi.fn(),
    setSelectionRange: vi.fn(),
    remove: vi.fn(),
  };
  const selection = { rangeCount: 0, getRangeAt: vi.fn(), removeAllRanges: vi.fn(), addRange: vi.fn() };
  const execCommand = vi.fn(() => (typeof execResult === "function" ? execResult() : execResult));
  const doc = {
    body: { appendChild: vi.fn() },
    activeElement: null,
    createElement: vi.fn(() => area),
    getSelection: () => selection,
    execCommand,
  } as unknown as Document;
  return { doc, area, execCommand, selection };
}

describe("copyText", () => {
  it("uses the clipboard API in a secure context", async () => {
    const writeText = vi.fn(async () => {});
    const { doc, execCommand } = fakeDoc(true);
    const env: CopyEnv = { isSecureContext: true, clipboard: { writeText }, doc };
    await expect(copyText("hello", env)).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("hello");
    expect(execCommand).not.toHaveBeenCalled();
  });

  it("falls back to execCommand in an insecure context and never touches navigator.clipboard", async () => {
    const writeText = vi.fn(async () => {});
    const { doc, area, execCommand } = fakeDoc(true);
    const env: CopyEnv = { isSecureContext: false, clipboard: { writeText }, doc };
    await expect(copyText("http://100.110.125.46:7878/link", env)).resolves.toBe(true);
    expect(writeText).not.toHaveBeenCalled();
    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(area.value).toBe("http://100.110.125.46:7878/link");
    expect(area.setAttribute).toHaveBeenCalledWith("readonly", "");
    expect(area.remove).toHaveBeenCalled();
  });

  it("falls back when the clipboard API exists but rejects", async () => {
    const writeText = vi.fn(async () => {
      throw new Error("NotAllowedError");
    });
    const { doc, execCommand } = fakeDoc(true);
    await expect(copyText("x", { isSecureContext: true, clipboard: { writeText }, doc })).resolves.toBe(true);
    expect(execCommand).toHaveBeenCalledWith("copy");
  });

  it("returns false when both routes fail, and restores the selection it found", async () => {
    const { doc, area, selection } = fakeDoc(false);
    await expect(copyText("x", { isSecureContext: false, doc })).resolves.toBe(false);
    expect(area.remove).toHaveBeenCalled();
    expect(selection.removeAllRanges).toHaveBeenCalled();
  });

  it("returns false when execCommand throws and there is no clipboard at all", async () => {
    const { doc } = fakeDoc(() => {
      throw new Error("unsupported");
    });
    await expect(copyText("x", { isSecureContext: false, clipboard: undefined, doc })).resolves.toBe(false);
  });

  it("returns false with no document to fall back into", async () => {
    await expect(copyText("x", { isSecureContext: false, doc: null })).resolves.toBe(false);
  });
});

describe("copy hints", () => {
  it("names Cmd+C on Mac and Ctrl+C elsewhere", () => {
    expect(copyShortcut("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")).toBe("⌘C");
    expect(copyShortcut("Mozilla/5.0 (X11; Linux x86_64)")).toBe("Ctrl+C");
    expect(manualCopyHint("Mozilla/5.0 (Macintosh)")).toContain("press ⌘C");
  });
});
