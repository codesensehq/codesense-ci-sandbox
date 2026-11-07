// SPDX-License-Identifier: MIT
import { beginContextNavigation } from "@/shared/lib/context-navigation";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const focusContext = vi.hoisted(() => ({ teamKey: "acme:default" }));
vi.mock("@/shared/lib/team-key", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/team-key")>()),
  getActiveTeamKey: () => focusContext.teamKey,
}));
// This unit suite isolates focus; real history integration is covered by context-switch seams.
vi.mock("./context-switch", () => ({
  recordContextChange: (_via: string, change: () => void) => {
    change();
    return true;
  },
}));

// Controllable fake viewer-store: setters mutate the backing state so the
// helpers' read-then-write logic is exercised end-to-end, and we can assert
// the resulting layout/collapse transitions.
const {
  state,
  setLayoutMode,
  setNavCollapsed,
  activateTrayWorkspace,
  setSelectedEpicGroup,
  revealTerminalInPlace,
} = vi.hoisted(() => {
  const s = {
    layoutMode: null as "console" | "shared" | "viewer" | null,
    lastTerminalMode: "shared" as "console" | "shared",
    terminalVisible: false,
    navCollapsed: {} as Record<string, boolean>,
    activeWorkspace: {} as Record<string, string>,
    rightPaneOpen: false,
    // OC-6: the state navigateEpicRail reads to reconstruct the rail imperatively.
    trayMode: {} as Record<string, "flat" | "grouped">,
    trayFullOrderByTeam: {} as Record<string, string[]>,
    // SV-GHI-2: the rest of what the real activeForegroundTiles reads (the tile order).
    trayBaseOrderByTeam: {} as Record<string, string[]>,
    workspaceOrderByTeam: {} as Record<string, string[]>,
    railKeyByWorkspaceId: {} as Record<string, Record<string, string>>,
    workspaces: {} as Record<string, { id: string; kind: string }[]>,
    softClosedWorkspaces: {} as Record<
      string,
      Record<string, { workspace: { id: string; kind: string } }>
    >,
    selectedEpicDirName: {} as Record<string, string>,
    mruWorkspaceStack: {} as Record<string, string[]>,
  };
  return {
    state: s,
    setLayoutMode: vi.fn((mode: "console" | "shared" | "viewer") => {
      s.layoutMode = mode;
      s.terminalVisible = mode !== "viewer";
    }),
    setNavCollapsed: vi.fn((teamKey: string, collapsed: boolean) => {
      s.navCollapsed[teamKey] = collapsed;
    }),
    // Mirror the real rule the tray relies on: activating a tray workspace makes
    // it active AND puts its terminal on screen — the state the focus coordinator
    // then reads. This is the integration `activateAndFocusTrayWorkspace` depends on.
    activateTrayWorkspace: vi.fn((teamKey: string, id: string) => {
      s.activeWorkspace[teamKey] = id;
      s.terminalVisible = true;
    }),
    setSelectedEpicGroup: vi.fn((teamKey: string, railKey: string) => {
      s.selectedEpicDirName[teamKey] = railKey;
    }),
    // SV-PBF-184: reveal the collapsed terminal without changing layout mode.
    revealTerminalInPlace: vi.fn(() => {
      s.layoutMode = "shared";
      s.terminalVisible = true;
    }),
  };
});

vi.mock("@/shared/stores/viewer-store", async (importOriginal) => ({
  // SV-GHI-2: the real empty-state predicate, run over the fake state below.
  activeForegroundTiles: (await importOriginal<typeof import("@/shared/stores/viewer-store")>())
    .activeForegroundTiles,
  useViewerStore: {
    getState: () => ({
      ...state,
      setLayoutMode,
      setNavCollapsed,
      activateTrayWorkspace,
      setSelectedEpicGroup,
      revealTerminalInPlace,
    }),
  },
}));

import {
  toggleTerminalWithFocus,
  toggleNavCollapsed,
  focusWorkspaceTerminal,
  focusVisibleTerminal,
  focusActiveWorkspaceTerminal,
  activateAndFocus,
  activateAndFocusTrayWorkspace,
  selectAndResumeEpicRail,
  navigateEpicRail,
  currentActivationGeneration,
  shouldFocusTerminalOnReveal,
  subscribeForegroundWorkspaceActivation,
  mayTakeUnrequestedFocus,
  revealOrToggleTerminal,
} from "./focus-cascade";
import { TERMINAL_POPUP_ATTR } from "@/shared/lib/focus-utils";

it("publishes every same-target foreground gesture while background activations stay silent", () => {
  const gestures = vi.fn();
  const unsubscribe = subscribeForegroundWorkspaceActivation(gestures);
  try {
    activateAndFocus({
      teamKey: "tray-follow-team",
      workspaceId: "already-active",
      focus: "preserve-focus",
    });
    activateAndFocus({
      teamKey: "tray-follow-team",
      workspaceId: "already-active",
      focus: "preserve-focus",
    });
    activateAndFocus({
      teamKey: "tray-follow-team",
      workspaceId: "background",
      presence: "background",
    });
    expect(gestures.mock.calls).toEqual([
      ["tray-follow-team", "already-active"],
      ["tray-follow-team", "already-active"],
    ]);
  } finally {
    unsubscribe();
  }
  activateAndFocus({
    teamKey: "tray-follow-team",
    workspaceId: "after-unmount",
    focus: "preserve-focus",
  });
  expect(gestures).toHaveBeenCalledTimes(2);
});

/** OC-5 (AC3): race-sensitive focus assertions run repeated burn-in. OC-1
 *  publishes no count, so it is pinned here. */
const FOCUS_RACE_BURN_IN = 25;

beforeEach(() => {
  focusContext.teamKey = "acme:default";
  state.layoutMode = null;
  state.lastTerminalMode = "shared";
  state.terminalVisible = false;
  state.navCollapsed = {};
  state.activeWorkspace = {};
  state.rightPaneOpen = false;
  state.trayMode = {};
  state.trayFullOrderByTeam = {};
  state.trayBaseOrderByTeam = {};
  state.workspaceOrderByTeam = {};
  state.railKeyByWorkspaceId = {};
  state.workspaces = {};
  state.softClosedWorkspaces = {};
  state.selectedEpicDirName = {};
  state.mruWorkspaceStack = {};
  vi.clearAllMocks();
  // Start every test with a clean DOM so focus assertions can never read a prior
  // test's leaked frames — a test that forgets its own fixture fails on an empty
  // body rather than silently inheriting stale HTML (order-independent).
  document.body.innerHTML = "";
  // rAF is only used for focus side-effects, which we don't assert here.
  vi.stubGlobal("requestAnimationFrame", () => 0);
});

/** A controllable requestAnimationFrame for focus suites. Install at the top of a
 *  describe: rAF callbacks are queued instead of run, so a test can either `flush()`
 *  them (DOM settling) or `take()` them UNRUN to stage a deterministic race (resolve a
 *  newer callback before an older one — the ordering a FIFO flush would hide). Registers
 *  its own before/afterEach and unstubs globals on teardown so the queue never leaks into
 *  sibling suites. */
function installControllableRaf(): { flush: () => void; take: () => FrameRequestCallback[] } {
  let queue: FrameRequestCallback[] = [];
  beforeEach(() => {
    queue = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      queue.push(cb);
      return queue.length;
    });
  });
  afterEach(() => vi.unstubAllGlobals());
  return {
    flush: () => {
      const pending = queue;
      queue = [];
      for (const cb of pending) cb(0);
    },
    take: () => {
      const pending = queue;
      queue = [];
      return pending;
    },
  };
}

/** Two side-by-side mounted pane frames (ws-a/ws-b → ta-a/ta-b), mirroring the mosaic
 *  DOM the host portals terminals into. ws-a is first in DOM order (the id-sorted tile
 *  order), so a test asserting ws-b proves the active-workspace path beat raw DOM order. */
function buildTwoWorkspaces(): void {
  document.body.innerHTML = `<div data-xterm-host>
    <div data-pane-id="ws-a"><div class="xterm"><textarea id="ta-a"></textarea></div></div>
    <div data-pane-id="ws-b"><div class="xterm"><textarea id="ta-b"></textarea></div></div></div>`;
}

describe("toggleNavCollapsed (SW-FX-8)", () => {
  it("flips an unset (expanded) nav to collapsed", () => {
    toggleNavCollapsed("acme:default");
    expect(setNavCollapsed).toHaveBeenCalledWith("acme:default", true);
  });

  it("flips a collapsed nav back to expanded", () => {
    state.navCollapsed["acme:default"] = true;
    toggleNavCollapsed("acme:default");
    expect(setNavCollapsed).toHaveBeenCalledWith("acme:default", false);
  });
});

describe("focusWorkspaceTerminal (SW-EW-33)", () => {
  it.each(['style="display:none"', 'inert aria-hidden="true"'])(
    "skips warm wrappers (%s) even when the inner display has its own style",
    (hidden) => {
      document.body.innerHTML = `<div data-xterm-host><div data-pane-id="ws-a">
      <div ${hidden}><div style="display:block"><div class="xterm"><textarea id="warm"></textarea></div></div></div>
      <div><div class="xterm"><textarea id="shown"></textarea></div></div>
    </div></div>`;
      focusVisibleTerminal();
      expect(document.activeElement?.id).toBe("shown");
      (document.activeElement as HTMLElement).blur();
      expect(focusWorkspaceTerminal("ws-a")).toBe(true);
      expect(document.activeElement?.id).toBe("shown");
    },
  );
  // Build two side-by-side pane frames, each holding a (visible) xterm textarea,
  // mirroring the mosaic DOM: a [data-pane-id] frame the host portals
  // the terminal body into.
  function buildPanes(): void {
    document.body.innerHTML = `<div data-xterm-host>
      <div data-pane-id="ws-a">
        <div class="xterm"><textarea id="ta-a"></textarea></div>
      </div>
      <div data-pane-id="ws-b">
        <div class="xterm"><textarea id="ta-b"></textarea></div>
      </div></div>`;
  }

  it("focuses the textarea inside the matching pane frame (not the first pane)", () => {
    buildPanes();
    expect(focusWorkspaceTerminal("ws-b")).toBe(true);
    expect(document.activeElement?.id).toBe("ta-b");
  });

  it("returns false when no frame matches the workspace id", () => {
    buildPanes();
    expect(focusWorkspaceTerminal("ws-zzz")).toBe(false);
  });

  it("skips a tab hidden via display:none and focuses the visible one", () => {
    document.body.innerHTML = `<div data-xterm-host>
      <div data-pane-id="ws-a">
        <div style="display:none"><div class="xterm"><textarea id="hidden"></textarea></div></div>
        <div style="display:block"><div class="xterm"><textarea id="visible"></textarea></div></div>
      </div></div>`;
    expect(focusWorkspaceTerminal("ws-a")).toBe(true);
    expect(document.activeElement?.id).toBe("visible");
  });

  it("returns false when the matched frame has no terminal (e.g. a viewer pane)", () => {
    document.body.innerHTML = `<div data-xterm-host><div data-pane-id="ws-a"><div>no terminal here</div></div></div>`;
    expect(focusWorkspaceTerminal("ws-a")).toBe(false);
  });

  it("skips a hidden team's frame (display:none ancestor) and focuses the visible one with the same id", () => {
    // Two teams mounted; the inactive one is display:none (App.tsx) and shares
    // the workspace id (e.g. both attach the same source repo → root:owner/repo).
    document.body.innerHTML = `<div data-xterm-host>
      <div style="display:none">
        <div data-pane-id="root:acme/repo">
          <div class="xterm"><textarea id="hidden-team"></textarea></div>
        </div>
      </div>
      <div>
        <div data-pane-id="root:acme/repo">
          <div class="xterm"><textarea id="visible-team"></textarea></div>
        </div>
      </div></div>`;
    expect(focusWorkspaceTerminal("root:acme/repo")).toBe(true);
    expect(document.activeElement?.id).toBe("visible-team");
  });
});

describe("focusActiveWorkspaceTerminal (SW-EW-33)", () => {
  it("focuses the terminal of the team's active workspace (after a jump/flip)", () => {
    // rAF runs the callback synchronously for this test.
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
    state.terminalVisible = true;
    state.activeWorkspace["acme:default"] = "ws-b";
    document.body.innerHTML = `<div data-xterm-host>
      <div data-pane-id="ws-a"><div class="xterm"><textarea id="ta-a"></textarea></div></div>
      <div data-pane-id="ws-b"><div class="xterm"><textarea id="ta-b"></textarea></div></div></div>`;
    focusActiveWorkspaceTerminal("acme:default");
    expect(document.activeElement?.id).toBe("ta-b");
  });

  it("no-ops when the team has no active workspace", () => {
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
    state.terminalVisible = true;
    document.body.innerHTML = `<div data-xterm-host><div data-pane-id="ws-a"><div class="xterm"><textarea id="ta-a"></textarea></div></div></div>`;
    expect(() => focusActiveWorkspaceTerminal("acme:default")).not.toThrow();
  });

  it("does NOT focus the terminal when it isn't on screen (terminal collapsed / viewer mode)", () => {
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
    state.terminalVisible = false; // terminal collapsed but mounted
    state.activeWorkspace["acme:default"] = "ws-b";
    document.body.innerHTML = `<div data-xterm-host>
      <input id="elsewhere" />
      <div data-pane-id="ws-b"><div class="xterm"><textarea id="ta-b"></textarea></div></div></div>`;
    const elsewhere = document.getElementById("elsewhere") as HTMLInputElement;
    elsewhere.focus();
    focusActiveWorkspaceTerminal("acme:default");
    // Cursor stays where it was (e.g. the document viewer), not yanked into the
    // invisible terminal.
    expect(document.activeElement?.id).toBe("elsewhere");
  });
});

describe("activateAndFocus (OC-5 coordinator)", () => {
  // Controllable rAF so a race can be staged deterministically (schedule A's focus,
  // then B's, then resolve out of order) — the "stale callback completes after a newer
  // activation" ordering NFR14 fences.
  const raf = installControllableRaf();

  it("focuses the active workspace terminal with the default foreground/focus-terminal intent", () => {
    state.terminalVisible = true;
    state.activeWorkspace["acme:default"] = "ws-b";
    buildTwoWorkspaces();
    activateAndFocus({ teamKey: "acme:default" });
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-b");
  });

  it("a newer file/layout navigation supersedes queued terminal focus", () => {
    state.terminalVisible = true;
    state.activeWorkspace["acme:default"] = "ws-b";
    buildTwoWorkspaces();
    activateAndFocus({ teamKey: "acme:default" });
    beginContextNavigation();
    const chosen = document.createElement("input");
    document.body.appendChild(chosen);
    chosen.focus();
    raf.flush();
    expect(document.activeElement).toBe(chosen);
  });

  it("does NOT focus for a background presence (a background warm never steals focus)", () => {
    state.terminalVisible = true;
    state.activeWorkspace["acme:default"] = "ws-b";
    buildTwoWorkspaces();
    (document.getElementById("ta-a") as HTMLTextAreaElement).focus();
    activateAndFocus({ teamKey: "acme:default", presence: "background", focus: "focus-terminal" });
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-a"); // unchanged
  });

  it.each(["preserve-focus", "focus-elsewhere"] as const)(
    "does NOT focus the terminal for %s intent (no focus theft)",
    (intent) => {
      state.terminalVisible = true;
      state.activeWorkspace["acme:default"] = "ws-b";
      buildTwoWorkspaces();
      (document.getElementById("ta-a") as HTMLTextAreaElement).focus();
      activateAndFocus({ teamKey: "acme:default", focus: intent });
      raf.flush();
      expect(document.activeElement?.id).toBe("ta-a"); // unchanged
    },
  );

  it("rejects a stale older focus that resolves AFTER a newer activation — burn-in", () => {
    state.terminalVisible = true;
    for (let i = 0; i < FOCUS_RACE_BURN_IN; i++) {
      buildTwoWorkspaces();
      activateAndFocus({ teamKey: "acme:default", workspaceId: "ws-a" }); // older
      activateAndFocus({ teamKey: "acme:default", workspaceId: "ws-b" }); // newer
      // Deliberately resolve the NEWER activation first, then the OLDER one LAST —
      // the interleaving a FIFO flush would hide. Without the generation fence the
      // late older callback would refocus A; the fence must reject it.
      const [olderCb, newerCb] = raf.take();
      newerCb(0);
      olderCb(0);
      expect(document.activeElement?.id).toBe("ta-b");
    }
  });

  it("does not focus an off-screen terminal (terminalVisible false)", () => {
    state.terminalVisible = false;
    state.activeWorkspace["acme:default"] = "ws-b";
    document.body.innerHTML = `<div data-xterm-host>
      <input id="elsewhere" />
      <div data-pane-id="ws-b"><div class="xterm"><textarea id="ta-b"></textarea></div></div></div>`;
    (document.getElementById("elsewhere") as HTMLInputElement).focus();
    activateAndFocus({ teamKey: "acme:default" });
    raf.flush();
    expect(document.activeElement?.id).toBe("elsewhere");
  });

  it("prefers the explicit workspaceId over the active slot", () => {
    state.terminalVisible = true;
    state.activeWorkspace["acme:default"] = "ws-b"; // active is B…
    buildTwoWorkspaces();
    activateAndFocus({ teamKey: "acme:default", workspaceId: "ws-a" }); // …but A is targeted
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-a");
  });

  it("a later focus-elsewhere activation supersedes a pending terminal focus (AC4 no steal-back)", () => {
    state.terminalVisible = true;
    document.body.innerHTML = `<div data-xterm-host>
      <input id="elsewhere" />
      <div data-pane-id="ws-a"><div class="xterm"><textarea id="ta-a"></textarea></div></div></div>`;
    (document.getElementById("elsewhere") as HTMLInputElement).focus();
    activateAndFocus({ teamKey: "acme:default", workspaceId: "ws-a" }); // pending terminal focus
    activateAndFocus({ teamKey: "acme:default", focus: "focus-elsewhere" }); // supersedes it
    raf.flush();
    // The pending terminal focus was fenced out; the non-terminal target keeps focus.
    expect(document.activeElement?.id).toBe("elsewhere");
  });

  it("generations are per-team — a newer activation in one team does not fence another", () => {
    state.terminalVisible = true;
    document.body.innerHTML = `<div data-xterm-host>
      <div data-pane-id="w1"><div class="xterm"><textarea id="t1"></textarea></div></div>
      <div data-pane-id="w2"><div class="xterm"><textarea id="t2"></textarea></div></div></div>`;
    activateAndFocus({ teamKey: "team1", workspaceId: "w1" }); // team1 first
    activateAndFocus({ teamKey: "team2", workspaceId: "w2" }); // team2 second
    const [cb1, cb2] = raf.take();
    // team1's focus resolves: a single GLOBAL counter would treat it as superseded
    // by team2 and bail (t1 never focuses); a per-team counter lets it land. Assert
    // t1 BEFORE touching team2 again, so a global-counter regression is caught here.
    focusContext.teamKey = "team1";
    cb1(0);
    expect(document.activeElement?.id).toBe("t1");
    focusContext.teamKey = "team2";
    cb2(0);
    expect(document.activeElement?.id).toBe("t2");
  });

  it("a stale team's deferred focus cannot land in the next team's same-id tile", () => {
    state.terminalVisible = true;
    document.body.innerHTML =
      '<input id="destination-control"><div data-xterm-host><div data-pane-id="same"><div class="xterm"><textarea id="other-team-terminal"></textarea></div></div></div>';
    activateAndFocus({ teamKey: "acme:default", workspaceId: "same" });
    focusContext.teamKey = "acme:ops";
    document.getElementById("destination-control")!.focus();
    raf.flush();
    expect(document.activeElement?.id).toBe("destination-control");
  });

  it("activateAndFocusTrayWorkspace activates (flipping terminalVisible) then focuses the selected terminal", () => {
    // terminalVisible starts false; the store rule must turn it on before focus.
    state.terminalVisible = false;
    buildTwoWorkspaces();
    activateAndFocusTrayWorkspace("acme:default", "ws-a");
    expect(activateTrayWorkspace).toHaveBeenCalledWith("acme:default", "ws-a");
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-a");
  });

  it("activateAndFocusTrayWorkspace no-ops for an empty workspace id", () => {
    buildTwoWorkspaces();
    activateAndFocusTrayWorkspace("acme:default", "");
    raf.flush();
    expect(activateTrayWorkspace).not.toHaveBeenCalled();
  });
});

describe("shouldFocusTerminalOnReveal / currentActivationGeneration (OC-9c panel-consult veto)", () => {
  // Drives through the real coordinator seam: activateAndFocus records the
  // authority, then the panel's veto query is asserted. rAF is stubbed to a
  // no-op — the record is written synchronously at mint, before any rAF. Each
  // case uses a distinct teamKey so the module-scoped map can't cross-contaminate.
  beforeEach(() => {
    vi.stubGlobal("requestAnimationFrame", () => 0);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("currentActivationGeneration is 0 before any activation and increments per activation", () => {
    expect(currentActivationGeneration("q-gen")).toBe(0);
    activateAndFocus({ teamKey: "q-gen", workspaceId: "ws-a" });
    expect(currentActivationGeneration("q-gen")).toBe(1);
    activateAndFocus({ teamKey: "q-gen", workspaceId: "ws-a" });
    expect(currentActivationGeneration("q-gen")).toBe(2);
  });

  it("generations are independent per team", () => {
    activateAndFocus({ teamKey: "q-gen-1", workspaceId: "ws-a" });
    activateAndFocus({ teamKey: "q-gen-1", workspaceId: "ws-a" });
    activateAndFocus({ teamKey: "q-gen-2", workspaceId: "ws-a" });
    expect(currentActivationGeneration("q-gen-1")).toBe(2);
    expect(currentActivationGeneration("q-gen-2")).toBe(1);
  });

  it("allows focus when the coordinator has never activated this team (no interference)", () => {
    expect(shouldFocusTerminalOnReveal("q-norecord", "ws-a", 0)).toBe(true);
  });

  it("allows focus when nothing newer landed since the attach stamp (this reveal is still current)", () => {
    activateAndFocus({ teamKey: "q-current", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("q-current");
    expect(shouldFocusTerminalOnReveal("q-current", "ws-a", stamp)).toBe(true);
  });

  it("suppresses a reveal begun during the current preserve-focus activation", () => {
    activateAndFocus({ teamKey: "q-preserved", workspaceId: "ws-a", focus: "preserve-focus" });
    const stamp = currentActivationGeneration("q-preserved");
    expect(shouldFocusTerminalOnReveal("q-preserved", "ws-a", stamp)).toBe(false);
  });

  it("a newer file/layout navigation vetoes delayed reveal even with the same activation generation", () => {
    activateAndFocus({ teamKey: "q-file", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("q-file");
    beginContextNavigation();
    expect(currentActivationGeneration("q-file")).toBe(stamp);
    expect(shouldFocusTerminalOnReveal("q-file", "ws-a", stamp)).toBe(false);
    // A subsequent deliberate terminal activation owns focus again.
    activateAndFocus({ teamKey: "q-file", workspaceId: "ws-a" });
    expect(shouldFocusTerminalOnReveal("q-file", "ws-a", stamp)).toBe(true);
  });

  it("allows focus when a newer activation ALSO targets this terminal (foreground focus-terminal, same ws)", () => {
    activateAndFocus({ teamKey: "q-samews", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("q-samews");
    activateAndFocus({ teamKey: "q-samews", workspaceId: "ws-a" }); // newer, same target
    expect(shouldFocusTerminalOnReveal("q-samews", "ws-a", stamp)).toBe(true);
  });

  it("suppresses focus when a newer activation targets a DIFFERENT workspace (the restore/reselect steal)", () => {
    activateAndFocus({ teamKey: "q-otherws", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("q-otherws");
    activateAndFocus({ teamKey: "q-otherws", workspaceId: "ws-b" }); // supersedes with a different target
    expect(shouldFocusTerminalOnReveal("q-otherws", "ws-a", stamp)).toBe(false);
  });

  it("suppresses focus when a newer activation is a non-focus-terminal intent", () => {
    activateAndFocus({ teamKey: "q-elsewhere", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("q-elsewhere");
    activateAndFocus({ teamKey: "q-elsewhere", focus: "focus-elsewhere" });
    expect(shouldFocusTerminalOnReveal("q-elsewhere", "ws-a", stamp)).toBe(false);
  });

  it("suppresses focus when a newer activation is a background warm", () => {
    activateAndFocus({ teamKey: "q-bg", workspaceId: "ws-a" });
    const stamp = currentActivationGeneration("q-bg");
    activateAndFocus({ teamKey: "q-bg", workspaceId: "ws-b", presence: "background" });
    expect(shouldFocusTerminalOnReveal("q-bg", "ws-a", stamp)).toBe(false);
  });
});

describe("selectAndResumeEpicRail (OC-6 shared pointer+keyboard tail)", () => {
  beforeEach(() => {
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("moves the selection AND resumes the MRU target (activate + focus)", () => {
    state.terminalVisible = true;
    document.body.innerHTML = `<div data-xterm-host><div data-pane-id="B-1"><div class="xterm"><textarea id="ta-b"></textarea></div></div></div>`;
    focusContext.teamKey = "t";
    const activated = selectAndResumeEpicRail("t", "beta", [{ id: "B-1", kind: "story" }], ["B-1"]);
    expect(activated).toBe(true);
    expect(setSelectedEpicGroup).toHaveBeenCalledWith("t", "beta");
    expect(activateTrayWorkspace).toHaveBeenCalledWith("t", "B-1");
    expect(document.activeElement?.id).toBe("ta-b");
  });

  it("moves the selection but does NOT activate for an all-ineligible (empty) rail", () => {
    const activated = selectAndResumeEpicRail("t", "beta", [], []);
    expect(activated).toBe(false);
    expect(setSelectedEpicGroup).toHaveBeenCalledWith("t", "beta"); // selection still moves
    expect(activateTrayWorkspace).not.toHaveBeenCalled(); // nothing to resume
  });
});

describe("navigateEpicRail (OC-6 keyboard prev/next-epic)", () => {
  const raf = installControllableRaf();

  // Grouped mode with three single-story rails alpha/beta/gamma, all eligible.
  function buildGroupedRails(): void {
    focusContext.teamKey = "t";
    state.trayMode["t"] = "grouped";
    state.terminalVisible = true;
    state.trayFullOrderByTeam["t"] = ["A-1", "B-1", "G-1"];
    state.railKeyByWorkspaceId["t"] = { "A-1": "alpha", "B-1": "beta", "G-1": "gamma" };
    state.workspaces["t"] = [
      { id: "A-1", kind: "story" },
      { id: "B-1", kind: "story" },
      { id: "G-1", kind: "story" },
    ];
    state.selectedEpicDirName["t"] = "alpha";
    state.mruWorkspaceStack["t"] = [];
    document.body.innerHTML = `<div data-xterm-host>
      <div data-pane-id="A-1"><div class="xterm"><textarea id="ta-a"></textarea></div></div>
      <div data-pane-id="B-1"><div class="xterm"><textarea id="ta-b"></textarea></div></div>
      <div data-pane-id="G-1"><div class="xterm"><textarea id="ta-g"></textarea></div></div></div>`;
  }

  it("next selects the adjacent rail and resumes+focuses its workspace", () => {
    buildGroupedRails();
    // OC-16: a real move reports `true`.
    expect(navigateEpicRail("t", "next")).toBe(true);
    raf.flush();
    expect(setSelectedEpicGroup).toHaveBeenCalledWith("t", "beta");
    expect(activateTrayWorkspace).toHaveBeenCalledWith("t", "B-1");
    expect(document.activeElement?.id).toBe("ta-b");
  });

  it("steps into a custom group through the same cached rail path", () => {
    buildGroupedRails();
    state.railKeyByWorkspaceId["t"]["B-1"] = " group:checks";
    expect(navigateEpicRail("t", "next")).toBe(true);
    raf.flush();
    expect(setSelectedEpicGroup).toHaveBeenCalledWith("t", " group:checks");
    expect(activateTrayWorkspace).toHaveBeenCalledWith("t", "B-1");
    expect(document.activeElement?.id).toBe("ta-b");
  });
  it("prev wraps from the first rail to the last", () => {
    buildGroupedRails();
    navigateEpicRail("t", "prev");
    raf.flush();
    expect(setSelectedEpicGroup).toHaveBeenCalledWith("t", "gamma");
    expect(document.activeElement?.id).toBe("ta-g");
  });

  it("is a no-op outside grouped mode", () => {
    buildGroupedRails();
    state.trayMode["t"] = "flat";
    // OC-16: non-grouped mode is blocked → reports `false`.
    expect(navigateEpicRail("t", "next")).toBe(false);
    raf.flush();
    expect(setSelectedEpicGroup).not.toHaveBeenCalled();
    expect(activateTrayWorkspace).not.toHaveBeenCalled();
  });

  it("reports blocked (false) when there is no other eligible rail", () => {
    buildGroupedRails();
    // Collapse to a single rail → next/prev has nowhere to go.
    state.trayFullOrderByTeam["t"] = ["A-1"];
    state.railKeyByWorkspaceId["t"] = { "A-1": "alpha" };
    state.workspaces["t"] = [{ id: "A-1", kind: "story" }];
    expect(navigateEpicRail("t", "next")).toBe(false);
    raf.flush();
    expect(setSelectedEpicGroup).not.toHaveBeenCalled();
  });

  it("skips an ineligible (empty) rail between the source and the next eligible one", () => {
    buildGroupedRails();
    // beta becomes empty (no members) → next from alpha must skip to gamma.
    state.trayFullOrderByTeam["t"] = ["A-1", "G-1"];
    state.railKeyByWorkspaceId["t"] = { "A-1": "alpha", "G-1": "gamma" };
    // (beta rail no longer has members, so it isn't in the reconstructed rails)
    navigateEpicRail("t", "next");
    raf.flush();
    expect(setSelectedEpicGroup).toHaveBeenCalledWith("t", "gamma");
    expect(document.activeElement?.id).toBe("ta-g");
  });

  it("falls back to the first rail when the stored selection is stale/absent (AC-8 parity)", () => {
    buildGroupedRails();
    // selectedEpicDirName points at a rail that no longer exists → the tray shows
    // rail[0] (alpha) selected; keyboard 'next' must move from alpha to beta, NOT
    // re-select alpha via a startIdx=-1 near-end resume.
    state.selectedEpicDirName["t"] = "gamma-gone";
    navigateEpicRail("t", "next");
    raf.flush();
    expect(setSelectedEpicGroup).toHaveBeenCalledWith("t", "beta");
    expect(document.activeElement?.id).toBe("ta-b");
  });

  it("inherits the coordinator generation fence under a rapid next→next race — burn-in", () => {
    for (let i = 0; i < FOCUS_RACE_BURN_IN; i++) {
      buildGroupedRails();
      navigateEpicRail("t", "next"); // alpha→beta: schedules focus B (older)
      navigateEpicRail("t", "next"); // beta→gamma: schedules focus G (newer)
      // Resolve NEWER (gamma) first, then the stale OLDER (beta) — the fence must
      // reject the late beta focus so the final epic (gamma) wins every iteration.
      const [olderCb, newerCb] = raf.take();
      newerCb(0);
      olderCb(0);
      expect(document.activeElement?.id).toBe("ta-g");
    }
  });
});

describe("toggleTerminalWithFocus (SW-FX-8)", () => {
  it("collapses a visible terminal to the viewer layout", () => {
    state.layoutMode = "shared";
    toggleTerminalWithFocus("acme:default");
    expect(setLayoutMode).toHaveBeenCalledWith("viewer");
  });

  it("restores the last non-viewer mode when the terminal is collapsed", () => {
    state.layoutMode = "viewer";
    state.lastTerminalMode = "console";
    toggleTerminalWithFocus("acme:default");
    expect(setLayoutMode).toHaveBeenCalledWith("console");
  });

  it("restores 'shared' when the last mode is the default", () => {
    state.layoutMode = "viewer";
    toggleTerminalWithFocus("acme:default");
    expect(setLayoutMode).toHaveBeenCalledWith("shared");
  });
});

describe("toggleTerminalWithFocus restore focus (SW-FX-87)", () => {
  // The SW-FX-8 describe above stubs rAF to a no-op (top-level beforeEach), so it
  // cannot observe focus. Restore focus lands in a rAF, so drive it through the
  // shared controllable-rAF harness.
  const raf = installControllableRaf();

  it("focuses the active workspace's terminal on restore, not the id-first tile (AC-1/2)", () => {
    state.layoutMode = "viewer"; // minimized → restore branch
    state.activeWorkspace["acme:default"] = "ws-b"; // last focused is NOT id-first
    buildTwoWorkspaces();
    toggleTerminalWithFocus("acme:default");
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-b");
  });

  it("falls back to focusVisibleTerminal when the team has no active workspace (AC-3)", () => {
    state.layoutMode = "viewer";
    // no activeWorkspace recorded (fresh/empty state)
    buildTwoWorkspaces();
    expect(() => {
      toggleTerminalWithFocus("acme:default");
      raf.flush();
    }).not.toThrow();
    expect(document.activeElement?.id).toBe("ta-a"); // first visible fallback
  });

  it("falls back to focusVisibleTerminal when the active workspace frame is not found (AC-4)", () => {
    state.layoutMode = "viewer";
    state.activeWorkspace["acme:default"] = "ws-gone"; // no matching frame (mid-mount)
    buildTwoWorkspaces();
    toggleTerminalWithFocus("acme:default");
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-a"); // fell back, not left on <body>
    expect(document.activeElement).not.toBe(document.body);
  });

  it("never blurs to body; a single workspace still receives focus on restore (AC-5)", () => {
    state.layoutMode = "viewer";
    state.activeWorkspace["acme:default"] = "solo";
    document.body.innerHTML = `<div data-xterm-host>
      <div data-pane-id="solo"><div class="xterm"><textarea id="ta-solo"></textarea></div></div></div>`;
    toggleTerminalWithFocus("acme:default");
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-solo");
    expect(document.activeElement).not.toBe(document.body);
  });

  it("skips a hidden team's same-id frame and focuses the visible team's terminal on restore (AC-1)", () => {
    // Two teams mounted; the inactive one is display:none (App.tsx) and shares the
    // workspace id (both attach the same source repo → root:owner/repo). Restore must
    // route through focusWorkspaceTerminal's team scoping, not the raw DOM-order scan,
    // so focus lands in the VISIBLE team — the collision is the "wrong workspace" hazard
    // this story exists to prevent.
    state.layoutMode = "viewer";
    state.activeWorkspace["acme:default"] = "root:acme/repo";
    document.body.innerHTML = `<div data-xterm-host>
      <div style="display:none">
        <div data-pane-id="root:acme/repo"><div class="xterm"><textarea id="hidden-team"></textarea></div></div>
      </div>
      <div>
        <div data-pane-id="root:acme/repo"><div class="xterm"><textarea id="visible-team"></textarea></div></div>
      </div></div>`;
    toggleTerminalWithFocus("acme:default");
    raf.flush();
    expect(document.activeElement?.id).toBe("visible-team");
  });
});

describe("revealOrToggleTerminal (SV-PBF-184)", () => {
  const raf = installControllableRaf();

  it("from CLI focus toggles the terminal closed via the shared helper", () => {
    state.layoutMode = "shared";
    document.body.innerHTML = `<div data-xterm-host><div data-pane-id="ws-a"><div class="xterm"><textarea id="ta-a"></textarea></div></div></div>`;
    document.getElementById("ta-a")!.focus();
    revealOrToggleTerminal("acme:default");
    expect(setLayoutMode).toHaveBeenCalledWith("viewer");
  });

  it("from non-CLI focus reveals a collapsed terminal and focuses the active workspace terminal", () => {
    state.layoutMode = "viewer";
    state.activeWorkspace["acme:default"] = "ws-a";
    document.body.innerHTML = `<input id="outside"><div data-xterm-host><div data-pane-id="ws-a"><div class="xterm"><textarea id="ta-a"></textarea></div></div></div>`;
    document.getElementById("outside")!.focus();
    revealOrToggleTerminal("acme:default");
    expect(revealTerminalInPlace).toHaveBeenCalledWith("acme:default");
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-a");
  });

  it("from non-CLI focus focuses a visible terminal without changing layout mode", () => {
    state.layoutMode = "shared";
    state.terminalVisible = true;
    state.activeWorkspace["acme:default"] = "ws-a";
    // A foreground tile, so this stays on the focus path (SV-GHI-2 hides only an empty CLI).
    state.workspaces["acme:default"] = [{ id: "ws-a", kind: "story" }];
    document.body.innerHTML = `<input id="outside"><div data-xterm-host><div data-pane-id="ws-a"><div class="xterm"><textarea id="ta-a"></textarea></div></div></div>`;
    document.getElementById("outside")!.focus();
    revealOrToggleTerminal("acme:default");
    expect(revealTerminalInPlace).not.toHaveBeenCalled();
    raf.flush();
    expect(document.activeElement?.id).toBe("ta-a");
    expect(setLayoutMode).not.toHaveBeenCalled();
  });

  it("does not create a terminal when none is visible and there is no active workspace", () => {
    state.layoutMode = "viewer";
    document.body.innerHTML = `<input id="outside"><div data-xterm-host></div>`;
    document.getElementById("outside")!.focus();
    expect(() => {
      revealOrToggleTerminal("acme:default");
      raf.flush();
    }).not.toThrow();
    expect(revealTerminalInPlace).toHaveBeenCalledWith("acme:default");
    expect(document.activeElement).toBe(document.getElementById("outside"));
  });

  // SV-GHI-2: with nothing in the CLI to focus, the shortcut hides the shown pane.
  it("hides a shown terminal from non-CLI focus when the team has no foreground tile", () => {
    state.layoutMode = "shared";
    state.terminalVisible = true;
    document.body.innerHTML = `<input id="outside"><div data-xterm-host></div>`;
    document.getElementById("outside")!.focus();
    revealOrToggleTerminal("acme:default");
    expect(setLayoutMode).toHaveBeenCalledWith("viewer");
    expect(revealTerminalInPlace).not.toHaveBeenCalled();
  });

  it("hides full-screen CLI with no foreground tile to the viewer layout", () => {
    state.layoutMode = "console";
    state.terminalVisible = true;
    document.body.innerHTML = `<input id="outside"><div data-xterm-host></div>`;
    document.getElementById("outside")!.focus();
    revealOrToggleTerminal("acme:default");
    expect(setLayoutMode).toHaveBeenCalledWith("viewer");
  });

  it("hides when the selected epic rail has no open member", () => {
    state.layoutMode = "shared";
    state.terminalVisible = true;
    state.trayMode["acme:default"] = "grouped";
    state.workspaces["acme:default"] = [{ id: "ws-a", kind: "story" }];
    state.railKeyByWorkspaceId["acme:default"] = { "ws-a": "epic-a" };
    state.selectedEpicDirName["acme:default"] = "epic-b";
    // An active id, but its tile lives on another rail: the pane shows the empty state.
    state.activeWorkspace["acme:default"] = "ws-a";
    document.body.innerHTML = `<input id="outside"><div data-xterm-host></div>`;
    document.getElementById("outside")!.focus();
    revealOrToggleTerminal("acme:default");
    expect(setLayoutMode).toHaveBeenCalledWith("viewer");
  });

  it("is a true toggle with no workspace: shows, hides, shows, and never moves focus", () => {
    state.layoutMode = "viewer";
    document.body.innerHTML = `<input id="outside"><div data-xterm-host></div>`;
    const outside = document.getElementById("outside")!;
    outside.focus();
    revealOrToggleTerminal("acme:default");
    raf.flush();
    expect(state.terminalVisible).toBe(true);
    revealOrToggleTerminal("acme:default");
    raf.flush();
    expect(state.layoutMode).toBe("viewer");
    revealOrToggleTerminal("acme:default");
    raf.flush();
    expect(state.terminalVisible).toBe(true);
    expect(document.activeElement).toBe(outside);
  });

  it("hiding the empty CLI keeps focus in an input inside the viewer content", () => {
    state.layoutMode = "shared";
    state.terminalVisible = true;
    document.body.innerHTML = `<div data-viewer-content tabindex="-1"><input id="doc"></div><div data-xterm-host></div>`;
    const doc = document.getElementById("doc")!;
    doc.focus();
    revealOrToggleTerminal("acme:default");
    raf.flush();
    expect(state.layoutMode).toBe("viewer");
    expect(document.activeElement).toBe(doc);
  });

  it("hiding the empty CLI with the scratch note open keeps focus in the viewer input", () => {
    state.layoutMode = "shared";
    state.terminalVisible = true;
    state.rightPaneOpen = true;
    document.body.innerHTML = `<div data-viewer-content tabindex="-1"><input id="doc"></div><div class="scratch-note-editor" tabindex="-1"></div><div data-xterm-host></div>`;
    const doc = document.getElementById("doc")!;
    doc.focus();
    revealOrToggleTerminal("acme:default");
    raf.flush();
    expect(state.layoutMode).toBe("viewer");
    expect(document.activeElement).toBe(doc);
  });
});

describe("mayTakeUnrequestedFocus", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  /** Focus a terminal's textarea, in a tile frame or in a terminal popup. */
  function focusTerminal(where: "tile" | "popup"): void {
    const host = document.createElement("div");
    if (where === "popup") host.setAttribute(TERMINAL_POPUP_ATTR, "");
    else host.setAttribute("data-pane-id", "ws-1");
    host.innerHTML = '<div class="xterm"><textarea></textarea></div>';
    document.body.appendChild(host);
    host.querySelector("textarea")!.focus();
  }

  it("a terminal of the same tile may hand focus on", () => {
    focusTerminal("tile");
    expect(mayTakeUnrequestedFocus("ws-1")).toBe(true);
  });

  it("is false while the quick view's terminal is being typed into, even for that tile", () => {
    focusTerminal("popup");
    expect(mayTakeUnrequestedFocus("ws-1")).toBe(false);
  });
});
