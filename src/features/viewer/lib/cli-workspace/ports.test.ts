// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  DomFocusId,
  ForegroundTileId,
  PtySessionId,
  RendererId,
  StoryWorkspaceId,
  TerminalTabId,
  ViewportId,
} from "./identities";
import { validateTransition } from "./lifecycle";
import type { CliWorkspaceCapability } from "./ports";

/**
 * OC-4 AC2/AC4 — the public capability is implementable using nothing but
 * contract types. This fake touches no DOM, React, Zustand, xterm, or Tauri
 * type; that it compiles and satisfies `CliWorkspaceCapability` is the proof that
 * the surface does not leak implementation types into its signatures. The
 * arch-boundary test enforces the same at the import level.
 */
function makeFakeCapability(): CliWorkspaceCapability {
  const activeWorkspace = StoryWorkspaceId("ws-1");
  const activeTab = TerminalTabId("tab-1");
  const session = PtySessionId("pty-1");
  const viewport = ViewportId("vp-1");

  return {
    activation: {
      activate: () => ({ ok: true, value: undefined }),
      eligibility: () => "eligible",
    },
    navigation: {
      navigate: () => ({ ok: true, value: activeWorkspace }),
      mostRecentEligible: () => activeWorkspace,
    },
    focus: {
      requestFocus: () => ({ ok: true, value: undefined }),
      activeTarget: () => ({
        workspace: activeWorkspace,
        terminalTab: activeTab,
        domFocus: DomFocusId("focus-1"),
      }),
    },
    viewports: {
      attach: () => ({ ok: true, value: viewport }),
      transition: (_vp, to) => {
        const err = validateTransition("viewport", "ready", to);
        return err ? { ok: false, error: err } : { ok: true, value: undefined };
      },
      readiness: () => ({
        readiness: "ready",
        renderer: RendererId("r-1"),
      }),
    },
    sessions: {
      session: () => session,
      close: () => ({ ok: true, value: undefined }),
    },
    zoom: {
      zoomTarget: () => ({ kind: "workspace", workspace: activeWorkspace }),
      setZoomTarget: () => ({ ok: true, value: undefined }),
    },
    diagnostics: {
      snapshot: () => ({
        teamId: "team-1",
        epicRailId: "rail-1",
        workspaceId: "ws-1",
        terminalTabId: "tab-1",
        ptySessionId: "pty-1",
        viewportId: "vp-1",
        focusIntent: "focus-terminal",
        presence: "foreground",
        hostVisibility: "visible",
        rendererReadiness: "ready",
        generation: 1,
        committedHead: 10,
        renderedHead: 10,
      }),
    },
  };
}

describe("OC-4 CliWorkspaceCapability is implementable from contract types (AC2/AC4)", () => {
  const cap = makeFakeCapability();

  it("activation carries explicit intent and reports eligibility", () => {
    const result = cap.activation.activate({
      workspace: StoryWorkspaceId("ws-1"),
      tile: ForegroundTileId("tile-1"),
      presence: "background",
      focus: "preserve-focus",
      generation: 3,
    });
    expect(result.ok).toBe(true);
    expect(cap.activation.eligibility(StoryWorkspaceId("ws-1"))).toBe("eligible");
  });

  it("accepts a legal viewport transition and rejects an illegal one through the typed boundary", () => {
    // The fake validates from `ready`: ready → detached is legal, but
    // ready → restored is not (restored is only reachable from hidden/detached).
    expect(cap.viewports.transition(ViewportId("vp-1"), "detached").ok).toBe(true);
    const bad = cap.viewports.transition(ViewportId("vp-1"), "restored");
    expect(bad.ok).toBe(false);
    if (!bad.ok) expect(bad.error.code).toBe("illegal-transition");
  });

  it("navigation exposes the MRU flip as a first-class axis", () => {
    const result = cap.navigation.navigate({ axis: "mru", direction: "next" });
    expect(result.ok).toBe(true);
    expect(cap.navigation.mostRecentEligible()).not.toBeNull();
  });

  it("safe diagnostic snapshot carries no content field", () => {
    const snap = cap.diagnostics.snapshot(ViewportId("vp-1"));
    const keys = Object.keys(snap);
    for (const forbidden of ["data", "output", "input", "command", "cwd", "env"]) {
      expect(keys).not.toContain(forbidden);
    }
  });
});
