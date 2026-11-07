// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { findTabLocationBySessionId, type TerminalTab } from "./viewer-store";

const tab = (sessionId: string, id = sessionId): TerminalTab => ({ id, name: id, sessionId });

describe("findTabLocationBySessionId (SW-BA-3 AC-1/AC-4)", () => {
  it("returns the {teamKey, workspaceId, tab} of the live tab carrying the session", () => {
    const map = {
      "team-1": { ws1: [tab("other")], ws2: [tab("sess-a", "foreground-sess-a")] },
      "team-2": { wsX: [tab("sess-b")] },
    };
    expect(findTabLocationBySessionId(map, "sess-a")).toEqual({
      teamKey: "team-1",
      workspaceId: "ws2",
      tab: { id: "foreground-sess-a", name: "foreground-sess-a", sessionId: "sess-a" },
    });
  });

  it("finds a tab that lives in a NON-first team/workspace (cross-workspace)", () => {
    const map = {
      "team-1": { ws1: [tab("nope")] },
      "team-2": { wsY: [tab("sess-b", "sess-b")] },
    };
    const loc = findTabLocationBySessionId(map, "sess-b");
    expect(loc?.teamKey).toBe("team-2");
    expect(loc?.workspaceId).toBe("wsY");
    expect(loc?.tab.id).toBe("sess-b");
  });

  it("returns undefined when no live tab carries the session", () => {
    const map = { "team-1": { ws1: [tab("x")] } };
    expect(findTabLocationBySessionId(map, "missing")).toBeUndefined();
  });

  it("returns undefined for an empty map", () => {
    expect(findTabLocationBySessionId({}, "sess-a")).toBeUndefined();
  });
});
