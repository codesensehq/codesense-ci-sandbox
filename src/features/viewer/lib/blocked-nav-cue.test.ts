// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  activeTileEl,
  CUE_DURATION_MS,
  flashBlockedNav,
  flashBlockedNavForWorkspace,
} from "./blocked-nav-cue";

const CUE_CLASSES = [
  "blocked-nav-pulse",
  "blocked-nav-nudge-left",
  "blocked-nav-nudge-right",
  "blocked-nav-nudge-up",
  "blocked-nav-nudge-down",
] as const;

describe("flashBlockedNav", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    document.body.innerHTML = "";
  });
  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
  });

  it("applies the cue class immediately and removes it after the timer", () => {
    const el = document.createElement("div");
    document.body.appendChild(el);
    flashBlockedNav(el, "pulse");
    expect(el.classList.contains("blocked-nav-pulse")).toBe(true);
    vi.advanceTimersByTime(500);
    expect(el.classList.contains("blocked-nav-pulse")).toBe(false);
  });

  it("maps each cue variant to its own class", () => {
    const el = document.createElement("div");
    flashBlockedNav(el, "nudge-left");
    expect(el.classList.contains("blocked-nav-nudge-left")).toBe(true);
    expect(el.classList.contains("blocked-nav-pulse")).toBe(false);
  });

  it("coalesces rapid repeats — the prior timer is cleared, not stacked", () => {
    const el = document.createElement("div");
    flashBlockedNav(el, "pulse");
    vi.advanceTimersByTime(100);
    // Re-flash before the first cue would have ended: restarts the same cue.
    flashBlockedNav(el, "pulse");
    expect(el.classList.contains("blocked-nav-pulse")).toBe(true);
    // Past when the FIRST timer would have fired — the class must still be present
    // (the first timer was cancelled, so no premature removal / no backlog).
    vi.advanceTimersByTime(200);
    expect(el.classList.contains("blocked-nav-pulse")).toBe(true);
    vi.advanceTimersByTime(500);
    expect(el.classList.contains("blocked-nav-pulse")).toBe(false);
  });

  it("switching variant on the same element replaces the class", () => {
    const el = document.createElement("div");
    flashBlockedNav(el, "pulse");
    flashBlockedNav(el, "nudge-right");
    expect(el.classList.contains("blocked-nav-pulse")).toBe(false);
    expect(el.classList.contains("blocked-nav-nudge-right")).toBe(true);
  });

  it("no-ops safely when the element is null", () => {
    expect(() => flashBlockedNav(null, "pulse")).not.toThrow();
  });
});

describe("activeTileEl", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("returns the tile frame whose data-pane-id matches", () => {
    const el = document.createElement("div");
    el.setAttribute("data-pane-id", "acme:default/w1");
    document.body.appendChild(el);
    expect(activeTileEl("acme:default/w1")).toBe(el);
  });

  it("returns null when no tile matches", () => {
    const el = document.createElement("div");
    el.setAttribute("data-pane-id", "other");
    document.body.appendChild(el);
    expect(activeTileEl("missing")).toBeNull();
  });
});

describe("flashBlockedNavForWorkspace", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("resolves the workspace's tile and flashes it", () => {
    const el = document.createElement("div");
    el.setAttribute("data-pane-id", "w1");
    document.body.appendChild(el);
    flashBlockedNavForWorkspace("w1", "pulse");
    expect(el.classList.contains("blocked-nav-pulse")).toBe(true);
  });

  it("no-ops when the workspace id is undefined (empty roster)", () => {
    expect(() => flashBlockedNavForWorkspace(undefined, "pulse")).not.toThrow();
  });

  it("no-ops when the workspace has no on-screen tile", () => {
    expect(() => flashBlockedNavForWorkspace("not-on-screen", "nudge-left")).not.toThrow();
  });
});

// The motion vs reduced-motion split is CSS-only (happy-dom can't evaluate
// @media), so these guard the source directly: a deletion of the reduced-motion
// branch or a drift of the duration would turn AC-3's accessibility promise off
// silently. Asserts against the real index.css.
describe("blocked-nav cue CSS (index.css)", () => {
  // vitest runs from the repo root; happy-dom's import.meta.url is not a file URL.
  const css = readFileSync(resolve(process.cwd(), "src/index.css"), "utf8");

  it("keeps CUE_DURATION_MS in lockstep with the CSS animation-duration", () => {
    expect(css).toContain(`animation-duration: ${CUE_DURATION_MS}ms`);
  });

  it("degrades every cue class to a static outline under prefers-reduced-motion", () => {
    // The reduced-motion guard is the @media segment that both names a cue class
    // and swaps to a static outline (the earlier segment holds the class defs).
    const reducedBlocks = css.split("@media (prefers-reduced-motion: reduce)");
    const cueGuard = reducedBlocks.find(
      (b) => b.includes("blocked-nav-pulse") && b.includes("outline:"),
    );
    expect(cueGuard, "a reduced-motion @media block must govern the cue classes").toBeDefined();
    for (const cls of CUE_CLASSES) expect(cueGuard).toContain(`.${cls}`);
    // Motion is dropped and replaced by a non-moving outline.
    expect(cueGuard).toContain("animation: none");
  });
});

describe("activeTileEl with a hidden team's frame of the same id", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("returns the shown frame, not the earlier hidden one", () => {
    document.body.innerHTML = `
      <div style="display: none"><div id="hidden" data-pane-id="ws-1"></div></div>
      <div><div id="shown" data-pane-id="ws-1"></div></div>`;
    expect(activeTileEl("ws-1")?.id).toBe("shown");
  });

  it("is null when every frame of the id is hidden", () => {
    document.body.innerHTML = `<div style="display: none"><div data-pane-id="ws-1"></div></div>`;
    expect(activeTileEl("ws-1")).toBeNull();
  });
});
