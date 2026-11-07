// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";
import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
}));

const REPO = "/test/repo";

describe("useViewerStore @P0", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useViewerStore.setState({
      openTabs: {},
      activeTab: {},
      terminalVisible: false,
      settings: {
        contentWidth: "normal",
        sidebarWidth: 240,
        appTheme: "codesense",
        viewerFontSize: "xl",
        treeFontSize: "large",
        terminal: "",
        terminalCustomCmd: "",
        editor: "",
        editorCustomCmd: "",
        terminalFontSize: "large",
        codeEditorFontSize: "normal",
        editorLineWrap: "auto",
        editorFolding: "auto",
        editorShowWhitespace: false,
        editorTabSize: 2,
        editorProseFont: false,
        terminalTheme: "github-dark",
        starshipPreset: "compact",
      },
      stateLoaded: false,
      openedTeams: [],
      navHistory: {},
      navIndex: {},
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  // ---------------------------------------------------------------------------
  // P0-014: Open / close / reorder tabs
  // ---------------------------------------------------------------------------
  describe("P0-014: Open / close / reorder tabs", () => {
    it("openFile adds a new tab and sets it active", () => {
      // Given: no tabs open for this repo
      // When: we open a file
      useViewerStore.getState().openFile(REPO, REPO, "docs/readme.md");

      // Then: the tab is added and marked active
      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]).toHaveLength(1);
      expect(state.openTabs[REPO]![0].path).toBe("docs/readme.md");
      expect(state.openTabs[REPO]![0].name).toBe("readme");
      expect(state.activeTab[REPO]).toBe("docs/readme.md");
    });

    it("openFile strips .html extension from tab name", () => {
      useViewerStore.getState().openFile(REPO, REPO, "mockups/dashboard.html");

      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]![0].name).toBe("dashboard");
    });

    it("openFile strips .htm extension from tab name", () => {
      useViewerStore.getState().openFile(REPO, REPO, "pages/index.htm");

      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]![0].name).toBe("index");
    });

    it("openFile does not duplicate an already-open tab", () => {
      // Given: a file is already open
      useViewerStore.getState().openFile(REPO, REPO, "docs/readme.md");

      // When: we open the same file again
      useViewerStore.getState().openFile(REPO, REPO, "docs/readme.md");

      // Then: still only one tab
      expect(useViewerStore.getState().openTabs[REPO]).toHaveLength(1);
    });

    it("openFile switches active tab to the newly opened file", () => {
      // Given: file A is open and active
      useViewerStore.getState().openFile(REPO, REPO, "a.md");

      // When: we open file B
      useViewerStore.getState().openFile(REPO, REPO, "b.md");

      // Then: active tab is now B, both tabs present
      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]).toHaveLength(2);
      expect(state.activeTab[REPO]).toBe("b.md");
    });

    it("closeTab removes the tab and selects an adjacent tab", () => {
      // Given: three tabs open, middle one active
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");
      useViewerStore.getState().openFile(REPO, REPO, "c.md");
      useViewerStore.getState().setActiveTab(REPO, REPO, "b.md");

      // When: we close the active tab (b.md)
      useViewerStore.getState().closeTab(REPO, REPO, "b.md");

      // Then: b is gone, active falls to c (the tab at the same index)
      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]!.map((t) => t.path)).toEqual(["a.md", "c.md"]);
      expect(state.activeTab[REPO]).toBe("c.md");
    });

    it("closeTab on the last remaining tab clears activeTab + navHistory + navIndex + openedViewerPaths for that team", () => {
      // Given: only one tab open
      useViewerStore.getState().openFile(REPO, REPO, "only.md");

      // When: we close it
      useViewerStore.getState().closeTab(REPO, REPO, "only.md");

      // Then: no tabs and all four team-keyed slices destructure-deleted.
      // T.4 Round 4 Patch 3: the last-tab branch mirrors closeAllTabs's
      // four-way clear so `navigateBack` doesn't resurrect the closed tab
      // and the keep-alive LRU drops the hidden viewer.
      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]).toEqual([]);
      expect(state.activeTab).not.toHaveProperty(REPO);
      expect(state.navHistory).not.toHaveProperty(REPO);
      expect(state.navIndex).not.toHaveProperty(REPO);
      expect(state.openedViewerPaths).not.toHaveProperty(REPO);
    });

    it("closeTab on the last remaining tab prevents navigateBack from resurrecting it (Round 4 Patch 3 motivation)", () => {
      // Given: history populated by two opens, then both closed except the
      // last; the navHistory[REPO] still references both files until the
      // last-tab close. Pre-patch, closing the only remaining tab left
      // navHistory populated and navigateBack would re-open the closed
      // file via commitNavTarget.
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");
      // Close `a` first so the tab strip is [b], then close `b`.
      useViewerStore.getState().closeTab(REPO, REPO, "a.md");
      useViewerStore.getState().closeTab(REPO, REPO, "b.md");

      // When: user hits Cmd+[ (navigateBack) after the last close.
      useViewerStore.getState().navigateBack(REPO, REPO);

      // Then: tab strip stays empty — no resurrection. (Pre-patch this
      // would have re-opened `a.md` from the stale history entry.)
      const state = useViewerStore.getState();
      expect(state.openTabs[REPO] ?? []).toEqual([]);
      expect(state.activeTab).not.toHaveProperty(REPO);
    });

    it("closeOtherTabs keeps only the specified tab", () => {
      // Given: three tabs open
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");
      useViewerStore.getState().openFile(REPO, REPO, "c.md");

      // When: we close others, keeping b
      useViewerStore.getState().closeOtherTabs(REPO, REPO, "b.md");

      // Then: only b remains and is active
      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]!.map((t) => t.path)).toEqual(["b.md"]);
      expect(state.activeTab[REPO]).toBe("b.md");
    });

    it("closeOtherTabs with a non-matching filePath destructure-deletes all 4 team-keyed slices (T.4 Round 5 TP1)", () => {
      // T.4 Round 4 Patch 6: when the kept-tab path isn't in openTabs (defensive
      // — current callers always pass a valid tab path), the writer
      // destructure-deletes `activeTab[teamKey]` rather than writing the
      // unknown path. Preserves the "absent ≠ empty" invariant.
      //
      // T.4 Round 5 TP1: the !tab branch was extended to also
      // destructure-delete navHistory / navIndex / openedViewerPaths so the
      // end-state matches closeAllTabs (zero tabs, no active tab, no nav
      // history, no LRU keep-alive). Original test asserted only activeTab.
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");

      useViewerStore.getState().closeOtherTabs(REPO, REPO, "/never-opened.md");

      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]).toEqual([]);
      // All four slices destructure-deleted (Object.keys check, not
      // `=== undefined`) — keys must be ABSENT from the slice, not present
      // with falsy values.
      expect(Object.keys(state.activeTab)).not.toContain(REPO);
      expect(Object.keys(state.navHistory)).not.toContain(REPO);
      expect(Object.keys(state.navIndex)).not.toContain(REPO);
      expect(Object.keys(state.openedViewerPaths)).not.toContain(REPO);
    });

    it("closeOtherTabs with a non-matching filePath does NOT fire revealOpenedFile (T.4 Round 5 TP1 reveal-gating)", () => {
      // T.4 Round 5 TP1: the post-set revealOpenedFile call is now gated
      // on a `hadTab` boolean captured before the set. With no matching
      // tab, reveal must not fire — otherwise statusNavExpandedPaths
      // would expand for a file that has no open tab.
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      // Seed statusNavExpandedPaths so we can observe whether reveal fires.
      useViewerStore.setState((s) => ({
        statusNavExpandedPaths: { ...s.statusNavExpandedPaths, [REPO]: [] },
      }));
      const beforeExpanded = useViewerStore.getState().statusNavExpandedPaths[REPO];

      useViewerStore.getState().closeOtherTabs(REPO, REPO, "/never-opened.md");

      // After destructure-delete, statusNavExpandedPaths[REPO] is gone too
      // (closeOtherTabs only clears the 4 slices listed above, not
      // statusNavExpandedPaths — that slice survives). If revealOpenedFile
      // had fired for "/never-opened.md", it would have mutated this slice
      // to expand ancestor paths. Assert the slice is identity-stable.
      const afterExpanded = useViewerStore.getState().statusNavExpandedPaths[REPO];
      expect(afterExpanded).toBe(beforeExpanded);
    });

    it("closeTab on the last remaining tab preserves sibling teams' nav state (T.4 Round 5 TP1 sibling isolation)", () => {
      // T.4 Round 5 TP1: the 4-slice destructure-delete in closeTab's
      // last-tab branch builds `{[teamKey]: _, ...rest}` for each slice.
      // A regression that returned `{}` instead of `rest…` for any of
      // the four would wipe every other team's bucket. Seed two teams
      // and assert sibling survives.
      const SIBLING = "/other/team";
      useViewerStore.setState({
        openTabs: {
          [REPO]: [{ path: "only.md", name: "only.md" }],
          [SIBLING]: [{ path: "sibling.md", name: "sibling.md" }],
        },
        activeTab: { [REPO]: "only.md", [SIBLING]: "sibling.md" },
        navHistory: { [REPO]: ["only.md"], [SIBLING]: ["sibling.md"] },
        navIndex: { [REPO]: 0, [SIBLING]: 0 },
        openedViewerPaths: { [REPO]: ["only.md"], [SIBLING]: ["sibling.md"] },
      });

      useViewerStore.getState().closeTab(REPO, REPO, "only.md");

      const state = useViewerStore.getState();
      // REPO is cleared in all four slices.
      expect(state.openTabs[REPO]).toEqual([]);
      expect(state.activeTab).not.toHaveProperty(REPO);
      expect(state.navHistory).not.toHaveProperty(REPO);
      expect(state.navIndex).not.toHaveProperty(REPO);
      expect(state.openedViewerPaths).not.toHaveProperty(REPO);
      // SIBLING survives — every slice retains its entry.
      expect(state.openTabs[SIBLING]?.map((t) => t.path)).toEqual(["sibling.md"]);
      expect(state.activeTab[SIBLING]).toBe("sibling.md");
      expect(state.navHistory[SIBLING]).toEqual(["sibling.md"]);
      expect(state.navIndex[SIBLING]).toBe(0);
      expect(state.openedViewerPaths[SIBLING]).toEqual(["sibling.md"]);
    });

    it("closeOtherTabs !tab preserves sibling teams' nav state (T.4 Round 5 TP1 sibling isolation)", () => {
      // Parallel to the closeTab sibling-isolation test above — the
      // closeOtherTabs !tab branch shares the same destructure-delete
      // pattern, so the same regression would affect it identically.
      const SIBLING = "/other/team";
      useViewerStore.setState({
        openTabs: {
          [REPO]: [{ path: "a.md", name: "a.md" }],
          [SIBLING]: [{ path: "sibling.md", name: "sibling.md" }],
        },
        activeTab: { [REPO]: "a.md", [SIBLING]: "sibling.md" },
        navHistory: { [REPO]: ["a.md"], [SIBLING]: ["sibling.md"] },
        navIndex: { [REPO]: 0, [SIBLING]: 0 },
        openedViewerPaths: { [REPO]: ["a.md"], [SIBLING]: ["sibling.md"] },
      });

      useViewerStore.getState().closeOtherTabs(REPO, REPO, "/never-opened.md");

      const state = useViewerStore.getState();
      // REPO cleared.
      expect(state.openTabs[REPO]).toEqual([]);
      expect(state.activeTab).not.toHaveProperty(REPO);
      expect(state.navHistory).not.toHaveProperty(REPO);
      expect(state.navIndex).not.toHaveProperty(REPO);
      expect(state.openedViewerPaths).not.toHaveProperty(REPO);
      // SIBLING untouched.
      expect(state.openTabs[SIBLING]?.map((t) => t.path)).toEqual(["sibling.md"]);
      expect(state.activeTab[SIBLING]).toBe("sibling.md");
      expect(state.navHistory[SIBLING]).toEqual(["sibling.md"]);
      expect(state.navIndex[SIBLING]).toBe(0);
      expect(state.openedViewerPaths[SIBLING]).toEqual(["sibling.md"]);
    });

    it("closeAllTabs empties tabs and removes active tab for the repo", () => {
      // Given: tabs open
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");

      // When
      useViewerStore.getState().closeAllTabs(REPO);

      // Then
      const state = useViewerStore.getState();
      expect(state.openTabs[REPO]).toEqual([]);
      expect(state.activeTab[REPO]).toBeUndefined();
    });

    it("reorderTab moves a tab from one index to another", () => {
      // Given: tabs [a, b, c]
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");
      useViewerStore.getState().openFile(REPO, REPO, "c.md");

      // When: move index 0 -> index 2
      useViewerStore.getState().reorderTab(REPO, 0, 2);

      // Then: [b, c, a]
      const paths = useViewerStore.getState().openTabs[REPO]!.map((t) => t.path);
      expect(paths).toEqual(["b.md", "c.md", "a.md"]);
    });

    it("setActiveTab changes the active tab without modifying openTabs", () => {
      // Given: two tabs, b is active
      useViewerStore.getState().openFile(REPO, REPO, "a.md");
      useViewerStore.getState().openFile(REPO, REPO, "b.md");

      // When: set a as active
      useViewerStore.getState().setActiveTab(REPO, REPO, "a.md");

      // Then
      expect(useViewerStore.getState().activeTab[REPO]).toBe("a.md");
      expect(useViewerStore.getState().openTabs[REPO]).toHaveLength(2);
    });
  });

  // ---------------------------------------------------------------------------
  // P0-026: Debounced persistence fires correctly
  // ---------------------------------------------------------------------------
  describe("P0-026: Debounced persistence fires correctly", () => {
    // Persisting is a no-op until the saved state has loaded (SW-FU-B2).
    beforeEach(() => {
      useViewerStore.setState({ stateLoaded: true });
    });

    it("persistState calls saveAppState after 300ms debounce", async () => {
      const { saveAppState, loadAppState } = await import("@/tauri/commands");

      // Given: mocks are cleared
      vi.mocked(saveAppState).mockClear();
      vi.mocked(loadAppState).mockClear();
      vi.mocked(loadAppState).mockResolvedValue("{}");

      // When: persistState is called
      useViewerStore.getState().persistState();

      // Then: saveAppState is NOT called synchronously
      expect(saveAppState).not.toHaveBeenCalled();

      // When: we advance timers past the 300ms debounce
      await vi.advanceTimersByTimeAsync(300);

      // Then: saveAppState was called
      expect(saveAppState).toHaveBeenCalledTimes(1);
    });

    it("multiple rapid persistState calls are debounced into one save", async () => {
      const { saveAppState, loadAppState } = await import("@/tauri/commands");
      vi.mocked(saveAppState).mockClear();
      vi.mocked(loadAppState).mockClear();
      vi.mocked(loadAppState).mockResolvedValue("{}");

      // When: persistState is called three times quickly
      useViewerStore.getState().persistState();
      useViewerStore.getState().persistState();
      useViewerStore.getState().persistState();

      await vi.advanceTimersByTimeAsync(300);

      // Then: only one save call
      expect(saveAppState).toHaveBeenCalledTimes(1);
    });

    it("actions that modify state trigger persistence", async () => {
      const { saveAppState, loadAppState } = await import("@/tauri/commands");
      vi.mocked(saveAppState).mockClear();
      vi.mocked(loadAppState).mockClear();
      vi.mocked(loadAppState).mockResolvedValue("{}");

      // When: openFile is called (it calls persistState internally)
      useViewerStore.getState().openFile(REPO, REPO, "test.md");

      // Then: after debounce, saveAppState fires
      await vi.advanceTimersByTimeAsync(300);
      expect(saveAppState).toHaveBeenCalled();
    });
  });

  // ---------------------------------------------------------------------------
  // Navigation history (back/forward)
  // ---------------------------------------------------------------------------
  describe("Navigation history", () => {
    it("navigateBack returns to previous file", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");
      s.openFile(REPO, REPO, "c.md");

      expect(useViewerStore.getState().activeTab[REPO]).toBe("c.md");

      useViewerStore.getState().navigateBack(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("b.md");
    });

    it("navigateForward returns to next file after back", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");
      s.openFile(REPO, REPO, "c.md");

      useViewerStore.getState().navigateBack(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("b.md");

      useViewerStore.getState().navigateForward(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("c.md");
    });

    it("openFile after back clears forward history", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");
      s.openFile(REPO, REPO, "c.md");

      useViewerStore.getState().navigateBack(REPO, REPO); // at b
      useViewerStore.getState().openFile(REPO, REPO, "d.md"); // clears forward (c)

      // Forward should be a no-op — c was cleared
      useViewerStore.getState().navigateForward(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("d.md");

      // History should be [a, b, d]
      expect(useViewerStore.getState().navHistory[REPO]).toEqual(["a.md", "b.md", "d.md"]);
    });

    it("back at start of history is a no-op", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");

      useViewerStore.getState().navigateBack(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("a.md");
    });

    it("forward at end of history is a no-op", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");

      useViewerStore.getState().navigateForward(REPO, REPO);
      expect(useViewerStore.getState().activeTab[REPO]).toBe("b.md");
    });

    it("caps history at 50 entries", () => {
      const s = useViewerStore.getState();
      for (let i = 0; i < 60; i++) {
        s.openFile(REPO, REPO, `file-${i}.md`);
      }

      const history = useViewerStore.getState().navHistory[REPO] ?? [];
      expect(history.length).toBe(50);
      expect(history[0]).toBe("file-10.md");
      expect(history[49]).toBe("file-59.md");
    });

    it("re-opening same file does not duplicate in history", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");
      s.openFile(REPO, REPO, "b.md"); // duplicate

      expect(useViewerStore.getState().navHistory[REPO]).toEqual(["a.md", "b.md"]);
    });

    it("closeAllTabs resets history for that repo", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");

      useViewerStore.getState().closeAllTabs(REPO);

      expect(useViewerStore.getState().navHistory[REPO]).toBeUndefined();
      expect(useViewerStore.getState().navIndex[REPO]).toBeUndefined();
    });

    it("navigateBack to closed tab re-adds it to openTabs", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");
      s.openFile(REPO, REPO, "c.md");

      // Close tab b
      useViewerStore.getState().closeTab(REPO, REPO, "b.md");
      expect(
        useViewerStore.getState().openTabs[REPO]?.find((t) => t.path === "b.md"),
      ).toBeUndefined();

      // Navigate back twice: c -> b
      useViewerStore.getState().navigateBack(REPO, REPO); // to b (closed)

      // b should be re-added
      const tabs = useViewerStore.getState().openTabs[REPO] ?? [];
      expect(tabs.find((t) => t.path === "b.md")).toBeDefined();
      expect(useViewerStore.getState().activeTab[REPO]).toBe("b.md");
    });

    // (Removed in C.6d) The previous "settings navigation pushes to history
    // and back returns to previous file (AC7)" test exercised the now-deleted
    // `__settings__` sentinel tab behavior. Settings is an app-wide panel
    // mounted at the App level (not a tab), so the case no longer reflects
    // production behavior. General navigateBack semantics are covered by
    // the surrounding navigateBack/navigateForward tests above.

    it("closeTab on active file reconciles navIndex", () => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.md");
      s.openFile(REPO, REPO, "b.md");
      s.openFile(REPO, REPO, "c.md");

      // navIndex is at 2 (c.md). Close c.md — active switches to b.md
      useViewerStore.getState().closeTab(REPO, REPO, "c.md");
      expect(useViewerStore.getState().activeTab[REPO]).toBe("b.md");

      // Now open d.md — should not produce duplicate b.md in history
      useViewerStore.getState().openFile(REPO, REPO, "d.md");
      const history = useViewerStore.getState().navHistory[REPO] ?? [];
      const bCount = history.filter((h) => h === "b.md").length;
      expect(bCount).toBe(1);
    });
  });

  // ---------------------------------------------------------------------------
  // P0-027: Settings update and persist
  // ---------------------------------------------------------------------------
  describe("P0-027: Settings update and persist", () => {
    // Persisting is a no-op until the saved state has loaded (SW-FU-B2).
    beforeEach(() => {
      useViewerStore.setState({ stateLoaded: true });
    });

    it("updateSettings merges partial settings into existing state", () => {
      // Given: default settings
      const before = useViewerStore.getState().settings;
      expect(before.contentWidth).toBe("normal");

      // When: we update contentWidth
      useViewerStore.getState().updateSettings({ contentWidth: "wide" });

      // Then: contentWidth changed, other settings unchanged
      const after = useViewerStore.getState().settings;
      expect(after.contentWidth).toBe("wide");
      expect(after.appTheme).toBe(before.appTheme);
      expect(after.sidebarWidth).toBe(before.sidebarWidth);
    });

    it("updateSettings can change multiple settings at once", () => {
      // When
      useViewerStore.getState().updateSettings({
        viewerFontSize: "small",
        terminalFontSize: "small",
        appTheme: "nord",
      });

      // Then
      const s = useViewerStore.getState().settings;
      expect(s.viewerFontSize).toBe("small");
      expect(s.terminalFontSize).toBe("small");
      expect(s.appTheme).toBe("nord");
    });

    it("updateSettings triggers persistence", async () => {
      const { saveAppState, loadAppState } = await import("@/tauri/commands");
      vi.mocked(saveAppState).mockClear();
      vi.mocked(loadAppState).mockClear();
      vi.mocked(loadAppState).mockResolvedValue("{}");

      // When
      useViewerStore.getState().updateSettings({ contentWidth: "full" });

      // Then: after debounce, save is called
      await vi.advanceTimersByTimeAsync(300);
      expect(saveAppState).toHaveBeenCalledTimes(1);

      // And the saved payload includes the updated settings
      const savedJson = vi.mocked(saveAppState).mock.calls[0][0];
      const saved = JSON.parse(savedJson);
      expect(saved.viewer.settings.contentWidth).toBe("full");
    });
  });
});
