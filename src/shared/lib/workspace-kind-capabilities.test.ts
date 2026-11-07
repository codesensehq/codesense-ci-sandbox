// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  WORKSPACE_KIND_CAPABILITIES,
  getKindCapabilities,
  showsStoryStatus,
  type WorkspaceKind,
} from "./workspace-kind-capabilities";

/**
 * SW-GS-2a — the registry encodes each kind's behavior so the 41-site migration is
 * zero-behavior-change (except the approved root closable/ungrouped change).
 * A drift here is a regression.
 */
describe("workspace-kind-capabilities registry", () => {
  it("has a row for every kind in the union and no stale kinds", () => {
    const kinds: WorkspaceKind[] = ["root", "story", "epic", "generic", "context"];
    expect(Object.keys(WORKSPACE_KIND_CAPABILITIES).sort()).toEqual([...kinds].sort());
    for (const k of kinds) {
      expect(getKindCapabilities(k)).toBe(WORKSPACE_KIND_CAPABILITIES[k]);
    }
  });

  it("root: closable + soft-closeable (2026-06-24), own ROOTS rail (SW-EW-38), dropped on load", () => {
    const c = getKindCapabilities("root");
    expect(c.closableByUser).toBe(true);
    expect(c.canSoftClose).toBe(true);
    expect(c.trayGroup).toBe("root");
    expect(c.foregroundTile).toBe(true);
    expect(c.loadMigration).toBe("drop");
  });

  it("story: status+HUD, worktree, frontmatter reconcile", () => {
    const c = getKindCapabilities("story");
    expect(c.statusBadge).toBe(true);
    expect(c.hudToggle).toBe(true);
    // SW-EW-46: story's tab LIST round-trips (fresh clean shells on reopen), like
    // generic/epic/root — the scrollback disk serialize was retired, so no smudge/extra tabs.
    expect(c.trayGroup).toBe("epic-member");
    expect(c.docPathSource).toBe("storyFilePath");
    expect(c.frontmatterReconcile).toBe(true);
    expect(c.surfaceWatchOnLoad).toBe(true);
    expect(c.worktreeStalenessCheck).toBe(true);
  });

  it("epic (renamed from integration): terminal workspace, HUD + doc title, keep-if-valid, epic dir", () => {
    const c = getKindCapabilities("epic");
    expect(c.closableByUser).toBe(true);
    expect(c.canSoftClose).toBe(true);
    expect(c.foregroundTile).toBe(true);
    expect(c.resolvesDocTitle).toBe(true);
    expect(c.hudToggle).toBe(true);
    expect(c.statusBadge).toBe(false);
    expect(c.docPathSource).toBe("epicDocPath");
    expect(c.trayGroup).toBe("epic-member");
    expect(c.loadMigration).toBe("keep-if-valid");
  });

  it("generic (SW-GW-1): visible real-dir terminal, ungrouped, KEEP + validate on reload, no chrome", () => {
    const c = getKindCapabilities("generic");
    expect(c.closableByUser).toBe(true);
    expect(c.canSoftClose).toBe(true);
    expect(c.foregroundTile).toBe(true);
    // OQ-1: a deliberately-opened workspace survives restart (unlike agent kinds).
    expect(c.loadMigration).toBe("keep");
    // …and because it's kept, its cwd is validated on load (deleted dir → error).
    expect(c.worktreeStalenessCheck).toBe(true);
    // OQ-3: visible + ungrouped (not epic-member → no grouping-code arm needed).
    expect(c.trayGroup).toBe("ungrouped");
    // No backing doc / status / HUD / frontmatter chrome.
    expect(c.statusBadge).toBe(false);
    expect(c.hudToggle).toBe(false);
    expect(c.resolvesDocTitle).toBe(false);
    expect(c.docPathSource).toBeNull();
    expect(c.frontmatterReconcile).toBe(false);
    expect(c.surfaceWatchOnLoad).toBe(false);
    // real user-chosen dir → skip the synthetic-clone gate.
    expect(c.skipsCloneGate).toBe(true);
  });

  it("all kinds are user-closable and foreground-tiled", () => {
    for (const k of ["root", "story", "epic", "generic", "context"] as const) {
      expect(getKindCapabilities(k).closableByUser).toBe(true);
      expect(getKindCapabilities(k).foregroundTile).toBe(true);
    }
  });

  it("only story carries the status badge", () => {
    expect(getKindCapabilities("story").statusBadge).toBe(true);
    for (const k of ["root", "epic"] as const) {
      expect(getKindCapabilities(k).statusBadge).toBe(false);
    }
  });

  // SW-VE-12: `sourceCapable` is the "every folder-rooted tab" gate the editor
  // + diff kebab + hotkeys now key on; `diffCapable` is the subset where the
  // Diff icon lights (its enclosing-repo surface is meaningful on the tab).
  // Both are consulted together by the viewer chrome, so a drift between the
  // registry and the chrome would be a regression.
  it("sourceCapable: TRUE on every live kind (editor renders against any folder-rooted cwd)", () => {
    for (const k of ["root", "story", "epic", "generic", "context"] as const) {
      expect(getKindCapabilities(k).sourceCapable).toBe(true);
    }
  });

  it("diffCapable: TRUE on root/story/generic; FALSE on epic (misleading enclosing-repo diff)", () => {
    for (const k of ["root", "story", "generic"] as const) {
      expect(getKindCapabilities(k).diffCapable).toBe(true);
    }
    expect(getKindCapabilities("epic").diffCapable).toBe(false);
  });

  // SW-AW-5 (AC-2): re-add the cwd/clone-gate capability cut in SW-AW-1 review.
  // Real-dir-cwd kinds (epic dir, repo root) skip the synthetic-clone attach gate;
  // only `story` (whose cwd depends on an attached clone) keeps it.
  it("skipsCloneGate: real-dir-cwd kinds (epic/root) skip the synthetic-clone gate; story keeps it", () => {
    expect(getKindCapabilities("epic").skipsCloneGate).toBe(true);
    // 2026-07-02: root resolves a real repo cwd, so the gate was already a
    // no-op for it — flipping it to skip is behavior-preserving (design deliberately).
    expect(getKindCapabilities("root").skipsCloneGate).toBe(true);
    // story's terminal cwd is a synthetic-clone that must be attached first — the
    // gate exists FOR story, so it must NOT skip.
    expect(getKindCapabilities("story").skipsCloneGate).toBe(false);
  });

  // SLW-FX-18: the single "shows a story-status control?" predicate shared by the
  // tile chrome (status dropdown) and the workspace pill (status glyph).
  describe("showsStoryStatus", () => {
    it("true only for a ready story WITH a resolved storyStatus", () => {
      expect(showsStoryStatus({ kind: "story", status: "ready", storyStatus: "in-progress" })).toBe(
        true,
      );
    });

    it("false when the story has no storyStatus yet", () => {
      expect(showsStoryStatus({ kind: "story", status: "ready", storyStatus: undefined })).toBe(
        false,
      );
    });

    it("false while the story is still creating (not ready)", () => {
      expect(showsStoryStatus({ kind: "story", status: "creating", storyStatus: "todo" })).toBe(
        false,
      );
    });

    it("false for every non-story (statusBadge=false) kind even when ready w/ a status", () => {
      for (const kind of ["root", "epic", "generic"] as const) {
        expect(showsStoryStatus({ kind, status: "ready", storyStatus: "review" })).toBe(false);
      }
    });
  });
});
