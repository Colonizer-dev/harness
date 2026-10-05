// The notify webhook's delivery health (issue #898). Rendered to static markup — the test
// environment has no DOM.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { WebhookDeliveries as Deliveries, WebhookDelivery } from "../../types";
import { WebhookDeliveries } from "./WebhookDeliveries";

const now = new Date("2026-10-05T12:00:00Z");

const letter = (over: Partial<WebhookDelivery> = {}): WebhookDelivery => ({
  key: "evt_0123456789abcdef0123456789abcdef.owner",
  event_id: "evt_0123456789abcdef0123456789abcdef",
  event: "pull_request",
  target: "owner",
  url: "https://hooks.example.com/colonizer",
  colony: null,
  attempts: 6,
  first_at: "2026-10-05T11:00:00Z",
  last_at: "2026-10-05T11:15:00Z",
  next_at: null,
  last_error: "the webhook answered 503 Service Unavailable",
  ...over,
});

const status = (over: Partial<Deliveries> = {}): Deliveries => ({
  pending: [],
  dead_letters: [],
  last_success_at: null,
  max_attempts: 6,
  ...over,
});

describe("WebhookDeliveries", () => {
  it("renders nothing before the webhook has done anything", () => {
    expect(renderToStaticMarkup(<WebhookDeliveries status={status()} now={now} />)).toBe("");
  });

  it("is a quiet line while every delivery goes through", () => {
    const out = renderToStaticMarkup(<WebhookDeliveries status={status({ last_success_at: "2026-10-05T11:58:00Z" })} now={now} />);
    expect(out).toContain("Webhook last delivered");
    expect(out).not.toContain("bg-warn-soft");
    expect(out).not.toContain("Replay");
  });

  it("warns about retries and lists each dead letter with Replay and Discard", () => {
    const out = renderToStaticMarkup(
      <WebhookDeliveries
        status={status({ pending: [letter({ key: "a", attempts: 2, next_at: "2026-10-05T12:01:00Z" })], dead_letters: [letter()] })}
        onReplay={() => {}}
        onDiscard={() => {}}
        now={now}
      />,
    );
    expect(out).toContain("1 retrying");
    expect(out).toContain("1 in the dead letter");
    expect(out).toContain("bg-warn-soft");
    expect(out).toContain("pull request");
    expect(out).toContain("6 attempts");
    expect(out).toContain("503 Service Unavailable");
    expect(out).toContain("Replay");
    expect(out).toContain("Discard");
  });

  it("disables the buttons of a letter whose replay is in flight", () => {
    const out = renderToStaticMarkup(
      <WebhookDeliveries status={status({ dead_letters: [letter()] })} busy={letter().key} onReplay={() => {}} onDiscard={() => {}} now={now} />,
    );
    expect(out.match(/disabled=""/g)?.length).toBe(2);
  });
});
