// Settings → Fleet (issue #686), rendered to static markup like the cockpit's other tests: each
// section renders from a pre-seeded fleet view (static markup runs no effects), the helpers behind
// the markup are pinned directly, and the routes are exercised on the mock api.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import { createMockApi } from "../mock";
import type { Api } from "../api";
import { ApiContext } from "../context";
import type { FleetState } from "../types";
import { FleetPane, InviteReveal, expiresText, joinedDay, runFleet, spacedCode } from "./FleetPane";

const wrap = (node: React.ReactNode) => renderToStaticMarkup(node);
// The pane reads the api itself; static markup runs no effects, so a stub context is enough.
const pane = (state: FleetState | null) => wrap(<ApiContext.Provider value={{} as Api}><FleetPane initial={state} /></ApiContext.Provider>);

const iso = (offsetMinutes: number) => new Date(Date.now() + offsetMinutes * 60_000).toISOString();
const NONE: FleetState = { role: "none", invites: [], pending: [], members: [], membership: null, joining: null };
const ownerState: FleetState = {
  role: "owner",
  invites: [{ id: "inv_1", expires_at: iso(9) }],
  pending: [{ id: "pen_1", name: "studio-2", url: "http://10.0.0.5:7878", confirm_code: "123456", expires_at: iso(11), status: "pending" }],
  members: [{ id: "mem_1", name: "rfc-annex", url: "http://10.0.0.6:7878", joined_at: iso(-60 * 24) }],
  membership: null,
  joining: null,
};
const memberState: FleetState = {
  role: "member",
  invites: [],
  pending: [],
  members: [],
  membership: { owner_url: "http://studio:7878", member_id: "mem_me", joined_at: iso(-3 * 60 * 24) },
  joining: null,
};
const joiningState: FleetState = { ...NONE, joining: { owner_url: "http://studio:7878", confirm_code: "424264", started_at: iso(-1) } };

describe("FleetPane sections", () => {
  it("as owner: shows the pending request's code with the compare warning, its invite and its member", () => {
    const html = pane(ownerState);
    expect(html).toContain(">Owner</span>");
    expect(html).toContain("123 456");
    expect(html).toContain("same code");
    expect(html).toContain(">Approve</button>");
    expect(html).toContain(">Reject</button>");
    expect(html).toContain(">Create invite</button>");
    expect(html).toContain("expires in 11 min");
    expect(html).toContain("rfc-annex");
    expect(html).toContain(">Remove</button>");
    expect(html).not.toContain(">Codes match</button>"); // an owner does not join
  });

  it("as member: shows the owner's URL since when, and keeps Leave behind its confirm", () => {
    const html = pane(memberState);
    expect(html).toContain(">Member</span>");
    expect(html).toContain("http://studio:7878");
    expect(html).toContain(">Leave fleet</button>");
    expect(html).not.toContain(">Confirm</button>");
    expect(html).not.toContain("Join a fleet");
  });

  it("while joining: shows this screen's code in two groups, with Codes match and Cancel", () => {
    const html = pane(joiningState);
    expect(html).toContain("424 264");
    expect(html).toContain(">Codes match</button>");
    expect(html).toContain(">Cancel</button>");
    expect(html).toContain("same six digits");
  });

  it("with no fleet and no join in flight: offers the join form", () => {
    const html = pane(NONE);
    expect(html).toContain("Join a fleet");
    expect(html).toContain('for="fleet-owner-url"');
    expect(html).toContain("Invite code");
    expect(html).toContain(">Join fleet</button>");
  });
});

describe("FleetPane helpers", () => {
  it("spaces the confirmation code in two groups, and leaves anything else alone", () => {
    expect(spacedCode("123456")).toBe("123 456");
    expect(spacedCode("12 3456")).toBe("12 3456");
    expect(spacedCode("abc")).toBe("abc");
  });

  it("counts the minutes to an expiry", () => {
    expect(expiresText(iso(12))).toBe("expires in 12 min");
    expect(expiresText(iso(-1))).toBe("expired");
  });

  it("reads the joined day, passing an unparseable stamp through", () => {
    const at = "2026-09-21T04:00:00Z";
    expect(joinedDay(at)).toBe(new Date(at).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }));
    expect(joinedDay("not a stamp")).toBe("not a stamp");
  });
});

describe("fleet on the mock api", () => {
  it("creating an invite shows the code once, and revoking drops it from the list", async () => {
    const api = createMockApi();
    const made = await api.createFleetInvite();
    expect(made.code).toHaveLength(16); // ≥ 80 bits; only this answer holds it
    expect(wrap(<InviteReveal invite={made} onDone={() => {}} />)).toContain(made.code);
    expect(wrap(<InviteReveal invite={made} onDone={() => {}} />)).toContain("Shown only now");
    expect((await api.fleet()).invites.map((row) => row.id)).toContain(made.id);
    await api.deleteFleetInvite(made.id);
    expect((await api.fleet()).invites.map((row) => row.id)).not.toContain(made.id);
  });

  it("approve calls the API for the pending id, and the row joins the members", async () => {
    const api = createMockApi();
    const id = (await api.fleet()).pending[0].id;
    expect(await runFleet(() => api.approveFleetPending(id))).toBeNull();
    const fleet = await api.fleet();
    expect(fleet.pending).toHaveLength(0);
    expect(fleet.members.map((m) => m.id)).toContain(id);
  });

  it("the join flow shows the confirm code, reads pending then joined, and leave ends the membership", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const api = createMockApi();
      await api.removeFleetMember((await api.fleet()).members[0].id); // no members left: joining unlocks
      expect((await api.fleet()).role).toBe("none");
      const started = await api.joinFleet({ owner_url: "http://studio:7878", code: "a".repeat(16) });
      expect(started.status).toBe("pending");
      // The pane renders the code this screen shows while waiting for the owner to decide.
      expect(pane(await api.fleet())).toContain(spacedCode(started.confirm_code));
      expect((await api.confirmFleetJoin()).status).toBe("pending"); // the owner has not approved yet
      await vi.advanceTimersByTimeAsync(7000);
      expect((await api.confirmFleetJoin()).status).toBe("joined");
      const fleet = await api.fleet();
      expect(fleet.role).toBe("member");
      expect(fleet.membership?.owner_url).toBe("http://studio:7878");
      expect(await runFleet(() => api.leaveFleet())).toBeNull();
      expect((await api.fleet()).role).toBe("none");
    } finally {
      vi.useRealTimers();
    }
  });

  it("answers one 404 for a bad invite code and one 409 for a fleet already joined", async () => {
    const api = createMockApi();
    await api.removeFleetMember((await api.fleet()).members[0].id);
    await expect(api.joinFleet({ owner_url: "http://studio:7878", code: "short" })).rejects.toMatchObject({ status: 404 });
    await expect(createMockApi().joinFleet({ owner_url: "http://studio:7878", code: "a".repeat(16) })).rejects.toMatchObject({ status: 409 });
  });
});
