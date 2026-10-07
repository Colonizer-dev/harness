// The storage alert card (issues #87, #220, #371): a failed write is red, and amber once a later
// write goes through; colony records lost at startup stay red although `ok` is true, since no later
// save brings them back. Rendered to static markup: the test environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { StorageAlert, dismissStorageAlert, storageAlertShown, visibleStorageAlert } from "./App";
import type { StorageHealth } from "./types";

const loadDamage: StorageHealth = {
  ok: true,
  kind: "load_damage",
  message: "/data/sessions.json could not be parsed (EOF while parsing) and was saved as /data/sessions.json.corrupt-1790000000",
  ts: "2026-09-20T23:00:00Z",
  failures: 1,
  recovered_at: null,
};

const recoveredWrite: StorageHealth = {
  ok: true,
  message: "disk quota exceeded",
  ts: "2026-09-20T23:05:00Z",
  failures: 3,
  recovered_at: "2026-09-20T23:12:00Z",
};

const markup = (storage: StorageHealth) => renderToStaticMarkup(<StorageAlert storage={storage} onDismiss={() => {}} />);

describe("StorageAlert", () => {
  it("shows load damage although writes are going through, and nothing when there is no alert", () => {
    expect(storageAlertShown(loadDamage)).toBe(true);
    expect(storageAlertShown({ ok: true })).toBe(false);
  });

  it("never renders load damage as a recovery: red, with the message naming the saved copy", () => {
    const out = markup(loadDamage);
    expect(out).toContain("The mothership could not load all its colony records");
    expect(out).toContain("sessions.json.corrupt-1790000000");
    expect(out).toContain('role="alert"');
    expect(out).toContain("Dismiss");
    expect(out).not.toContain("writing to disk again");
    expect(out).not.toContain("failed write");
  });

  it("still turns a recovered write failure amber, reading a missing kind as a write", () => {
    expect(storageAlertShown(recoveredWrite)).toBe(true);
    for (const storage of [recoveredWrite, { ...recoveredWrite, kind: "write" as const }]) {
      const out = markup(storage);
      expect(out).toContain("The mothership is writing to disk again");
      expect(out).toContain('role="status"');
    }
    expect(markup({ ...recoveredWrite, ok: false, recovered_at: null })).toContain("The mothership could not write to disk");
  });

  it("keeps dismissed load damage hidden when the mothership shows it again after a write failure recovers", () => {
    let dismissed: ReadonlySet<string> = new Set();
    expect(visibleStorageAlert(loadDamage, dismissed)).toBe(loadDamage);
    dismissed = dismissStorageAlert(dismissed, loadDamage);
    expect(visibleStorageAlert(loadDamage, dismissed)).toBeNull();

    // A later write failure has its own ts, so it shows although load damage was dismissed.
    const failing: StorageHealth = { ...recoveredWrite, ok: false, recovered_at: null };
    expect(visibleStorageAlert(failing, dismissed)).toBe(failing);
    dismissed = dismissStorageAlert(dismissed, failing);
    expect(visibleStorageAlert(failing, dismissed)).toBeNull();

    // Once the write recovers the mothership reports the load damage again, same ts: still dismissed.
    expect(visibleStorageAlert({ ...loadDamage }, dismissed)).toBeNull();
    expect(visibleStorageAlert(undefined, dismissed)).toBeNull();
  });
});
