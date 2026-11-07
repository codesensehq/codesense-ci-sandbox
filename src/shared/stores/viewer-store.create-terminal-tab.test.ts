// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { createTerminalTab, pickNavRegisterSessionId, type TerminalTab } from "./viewer-store";

// SW-GW-6: `createTerminalTab` mints a tab to open with an optional `title`.
describe("createTerminalTab (SW-GW-6 title)", () => {
  it("leaves the name to the host (`Terminal N`) when no title is given", () => {
    const tab = createTerminalTab();
    expect(tab.name).toBeUndefined();
    expect(tab.sessionId).toMatch(/^shell-/);
  });

  it("uses `title` for the display name when provided", () => {
    const tab = createTerminalTab("Architecture Review");
    expect(tab.name).toBe("Architecture Review");
    // title is cosmetic only — the session id is still a fresh uuid.
    expect(tab.sessionId).toMatch(/^shell-/);
  });

  it("mints a unique session id per call (no collision across tabs)", () => {
    const a = createTerminalTab("Same Title");
    const b = createTerminalTab("Same Title");
    expect(a.sessionId).not.toBe(b.sessionId);
  });
});

// SW-GW-8 (AC-1): which session a nav-open folds under the workspace's BARE handle.
describe("pickNavRegisterSessionId (SW-GW-8)", () => {
  const tab = (over: Partial<TerminalTab>): TerminalTab => ({
    id: "t",
    name: "n",
    sessionId: "s",
    ...over,
  });

  it("picks the bare shell on a fresh nav open (no agent tab yet)", () => {
    expect(pickNavRegisterSessionId([tab({ sessionId: "shell-1" })])).toBe("shell-1");
  });

  it("prefers the MAIN agent tab (role without #) over a bare shell", () => {
    const tabs = [
      tab({ sessionId: "shell-1" }),
      tab({ sessionId: "main-1", role: "story-dev:SW-A" }),
    ];
    expect(pickNavRegisterSessionId(tabs)).toBe("main-1");
  });

  it("NEVER folds a named SW-GW-6 session (role with #) under the bare handle", () => {
    // Only a named session present → nothing registers under the bare handle
    // (the named session is already registered under its own `handle#label`).
    const named = [tab({ sessionId: "named-1", role: "story-dev:SW-A#review" })];
    expect(pickNavRegisterSessionId(named)).toBeUndefined();
    // With a shell alongside a named session, the SHELL wins — not the named one.
    const mixed = [
      tab({ sessionId: "named-1", role: "story-dev:SW-A#review" }),
      tab({ sessionId: "shell-1" }),
    ];
    expect(pickNavRegisterSessionId(mixed)).toBe("shell-1");
  });

  it("returns undefined for no tabs", () => {
    expect(pickNavRegisterSessionId([])).toBeUndefined();
  });
});
