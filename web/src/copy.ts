// Copy text to the clipboard from any origin. navigator.clipboard exists only in a secure context
// (https or localhost), so a mothership reached over plain HTTP on a tailnet IP has no clipboard API
// and every copy button would silently do nothing. The fallback is a hidden read-only textarea with
// execCommand("copy"), which works in an insecure context from a user gesture. When that fails too,
// the caller gets false and tells the person to copy by hand.

/** What copyText needs from its surroundings; injectable so the paths can be tested without a DOM. */
export interface CopyEnv {
  isSecureContext: boolean;
  clipboard?: { writeText(text: string): Promise<void> } | undefined;
  doc?: Document | null;
}

export function browserCopyEnv(): CopyEnv {
  return {
    isSecureContext: typeof window !== "undefined" && window.isSecureContext === true,
    clipboard: typeof navigator !== "undefined" ? navigator.clipboard : undefined,
    doc: typeof document !== "undefined" ? document : null,
  };
}

/** Resolves true only when the text really reached the clipboard. */
export async function copyText(text: string, env: CopyEnv = browserCopyEnv()): Promise<boolean> {
  if (env.isSecureContext && typeof env.clipboard?.writeText === "function") {
    try {
      await env.clipboard.writeText(text);
      return true;
    } catch {
      // Permission refused or the document lost focus: the textarea route may still work.
    }
  }
  return execCopy(text, env.doc ?? null);
}

function execCopy(text: string, doc: Document | null): boolean {
  if (!doc || !doc.body || typeof doc.execCommand !== "function") return false;
  const selection = typeof doc.getSelection === "function" ? doc.getSelection() : null;
  const savedRange = selection && selection.rangeCount > 0 ? selection.getRangeAt(0).cloneRange() : null;
  const previousFocus = (doc.activeElement as HTMLElement | null) ?? null;

  const area = doc.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.setAttribute("aria-hidden", "true");
  area.style.position = "fixed";
  area.style.top = "0";
  area.style.left = "0";
  area.style.width = "1px";
  area.style.height = "1px";
  area.style.opacity = "0";
  area.style.pointerEvents = "none";
  doc.body.appendChild(area);

  let ok = false;
  try {
    area.focus();
    area.select();
    area.setSelectionRange(0, text.length); // iOS ignores select() alone
    ok = doc.execCommand("copy") === true;
  } catch {
    ok = false;
  } finally {
    area.remove();
    if (selection) {
      selection.removeAllRanges();
      if (savedRange) selection.addRange(savedRange);
    }
    previousFocus?.focus?.();
  }
  return ok;
}

/** The copy shortcut to name in the manual-copy hint. */
export function copyShortcut(userAgent: string = typeof navigator !== "undefined" ? navigator.userAgent : ""): string {
  return /Macintosh|Mac OS X/.test(userAgent) ? "⌘C" : "Ctrl+C";
}

/** The message shown when copyText returned false. */
export function manualCopyHint(userAgent?: string): string {
  return `Couldn't copy here. Select the text and press ${copyShortcut(userAgent)} to copy`;
}
