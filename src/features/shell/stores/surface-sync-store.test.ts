// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";
import { renderHook } from "@testing-library/react";

import { useSurfaceSyncStore, useSurfaceSyncStatus } from "./surface-sync-store";

const store = () => useSurfaceSyncStore.getState();
const PATH = "/abs/worktree-a";

describe("surface-sync-store (SW-TS-3 AC-2/AC-3)", () => {
  beforeEach(() => {
    useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
  });

  it("markStarted sets syncing keyed by code_repo_path", () => {
    store().markStarted(PATH, 1);
    expect(store().byPath[PATH]).toEqual({ state: "syncing", waveId: 1 });
  });

  describe("SW-FX-89: reason label + multi-surface fan-out", () => {
    it("markStarted records the wave reason label for the syncing hover", () => {
      store().markStarted(PATH, 1, "Preparing context…");
      expect(store().byPath[PATH]).toEqual({
        state: "syncing",
        waveId: 1,
        reasonLabel: "Preparing context…",
      });
    });

    it("markStartedMany marks every affected surface syncing with the shared reason", () => {
      store().markStartedMany(["/a", "/b", "/c"], 4, "Switching team…");
      for (const p of ["/a", "/b", "/c"]) {
        expect(store().byPath[p]).toEqual({
          state: "syncing",
          waveId: 4,
          reasonLabel: "Switching team…",
        });
      }
    });

    it("markStartedMany is ONE store notification for the whole fan-out", () => {
      let notifications = 0;
      const unsub = useSurfaceSyncStore.subscribe(() => {
        notifications += 1;
      });
      store().markStartedMany(["/a", "/b", "/c"], 1, "Initial sync…");
      unsub();
      expect(notifications).toBe(1);
    });

    it("markStartedMany applies the per-path stale/supersede guard", () => {
      store().markCompleted("/a", 5, { ok: true }); // /a settled at a newer wave
      store().markStartedMany(["/a", "/b"], 3, "x"); // wave 3 is stale for /a, new for /b
      expect(store().byPath["/a"]).toEqual({ state: "synced", waveId: 5 }); // stale started ignored
      expect(store().byPath["/b"]).toEqual({ state: "syncing", waveId: 3, reasonLabel: "x" });
    });

    it("markStartedMany no-ops without a notification when every started is stale", () => {
      store().markCompleted("/a", 5, { ok: true });
      store().markCompleted("/b", 5, { ok: true });
      let notifications = 0;
      const unsub = useSurfaceSyncStore.subscribe(() => {
        notifications += 1;
      });
      store().markStartedMany(["/a", "/b"], 2, "x");
      unsub();
      expect(notifications).toBe(0);
    });

    // AC-4: a large fan-out is ONE store notification, and an unaffected surface's
    // status object stays referentially identical — so its per-path selector
    // (`useSurfaceWaveStatus`) sees `Object.is` equality and the unaffected chip
    // never re-renders. This is the scale guarantee at ~50 open workspaces.
    it("AC-4: a 50-surface fan-out is one notification and leaves unaffected slots untouched", () => {
      const paths = Array.from({ length: 50 }, (_, i) => `/abs/wt-${i}`);
      store().markCompleted("/abs/other", 1, { ok: true }); // an unrelated settled surface
      const otherBefore = store().byPath["/abs/other"];
      let notifications = 0;
      const unsub = useSurfaceSyncStore.subscribe(() => {
        notifications += 1;
      });
      store().markStartedMany(paths, 2, "Preparing context…");
      unsub();
      expect(notifications).toBe(1); // ONE write for all 50 affected surfaces
      const syncing = Object.values(store().byPath).filter((v) => v.state === "syncing");
      expect(syncing).toHaveLength(50);
      // Unaffected surface: same object reference → its selector won't re-render.
      expect(store().byPath["/abs/other"]).toBe(otherBefore);
    });
  });

  it("a clean completion finalizes synced", () => {
    store().markStarted(PATH, 1);
    store().markCompleted(PATH, 1, { ok: true });
    expect(store().byPath[PATH]).toEqual({ state: "synced", waveId: 1 });
  });

  it("a failed completion carries the reason", () => {
    store().markStarted(PATH, 2);
    store().markCompleted(PATH, 2, { ok: false, errorMessage: "disk full" });
    expect(store().byPath[PATH]).toEqual({
      state: "failed",
      waveId: 2,
      errorMessage: "disk full",
    });
  });

  it("a stale (superseded) completed event never clobbers a newer wave's state", () => {
    // Newer wave (id 5) is already syncing for this surface…
    store().markStarted(PATH, 5);
    // …when a late completed event for the OLD wave (id 3) arrives.
    store().markCompleted(PATH, 3, { ok: true });
    // The newer syncing state must survive.
    expect(store().byPath[PATH]).toEqual({ state: "syncing", waveId: 5 });
  });

  it("a newer wave supersedes an older surface state", () => {
    store().markCompleted(PATH, 1, { ok: false, errorMessage: "boom" });
    store().markStarted(PATH, 2); // a new wave
    expect(store().byPath[PATH]).toEqual({ state: "syncing", waveId: 2 });
  });

  it("a late started event for an already-completed wave never downgrades it", () => {
    // IPC reorder: completed lands before started for the SAME wave id.
    store().markCompleted(PATH, 4, { ok: true });
    store().markStarted(PATH, 4);
    expect(store().byPath[PATH]).toEqual({ state: "synced", waveId: 4 });
  });

  it("surfaces are independent — one surface's wave never touches another", () => {
    store().markStarted("/abs/a", 1);
    store().markStarted("/abs/b", 1);
    store().markCompleted("/abs/a", 1, { ok: true });
    expect(store().byPath["/abs/a"]?.state).toBe("synced");
    expect(store().byPath["/abs/b"]?.state).toBe("syncing");
  });

  // SLW-CO-6 (AC-6): epic per-repo assembly failures live in their own slot so a
  // racing EpicInheritance `synced` (same surface key) can't mask them.
  describe("assembly-failure channel (SLW-CO-6 AC-6)", () => {
    it("markAssemblyFailure records the reason; the effective status is failed", () => {
      store().markAssemblyFailure(PATH, "2 repos couldn't assemble: a/b, a/c");
      expect(store().assemblyFailure[PATH]).toBe("2 repos couldn't assemble: a/b, a/c");
      const { result } = renderHook(() => useSurfaceSyncStatus(PATH));
      expect(result.current).toEqual({
        state: "failed",
        waveId: 0,
        errorMessage: "2 repos couldn't assemble: a/b, a/c",
      });
    });

    it("a racing inheritance `synced` does NOT mask an assembly failure (the race the slot fixes)", () => {
      // The inheritance wave for the epic dir completes OK…
      store().markStarted(PATH, 3);
      store().markCompleted(PATH, 3, { ok: true });
      // …but per-repo assembly failed (arrives after, same surface key).
      store().markAssemblyFailure(PATH, "repo a/b couldn't assemble");
      // The chip must still show FAILED (assembly takes precedence), not synced.
      const { result } = renderHook(() => useSurfaceSyncStatus(PATH));
      expect(result.current?.state).toBe("failed");
      expect(result.current?.errorMessage).toBe("repo a/b couldn't assemble");
    });

    it("a LATER inheritance wave does NOT clear the assembly failure (assembly owns its slot)", () => {
      // The masking regression the review caught: `ensure_epic_workspace` /
      // reconcile fires its own EpicInheritance wave AFTER assembly failed —
      // clearing on `markStarted` would flip the chip green with worktrees still
      // missing. `markStarted` must leave `assemblyFailure` untouched.
      store().markAssemblyFailure(PATH, "boom");
      store().markStarted(PATH, 7);
      store().markCompleted(PATH, 7, { ok: true });
      expect(store().assemblyFailure[PATH]).toBe("boom");
      const { result } = renderHook(() => useSurfaceSyncStatus(PATH));
      expect(result.current?.state).toBe("failed");
    });

    it("clearAssemblyFailure clears the slot (a fresh successful re-assembly)", () => {
      store().markAssemblyFailure(PATH, "boom");
      store().clearAssemblyFailure(PATH);
      expect(store().assemblyFailure[PATH]).toBeUndefined();
      const { result } = renderHook(() => useSurfaceSyncStatus(PATH));
      expect(result.current).toBeUndefined(); // no wave state either → calm
    });

    it("assembly failures are per-surface (one epic's failure never touches another)", () => {
      store().markAssemblyFailure("/abs/epic-a", "boom");
      expect(store().assemblyFailure["/abs/epic-b"]).toBeUndefined();
    });
  });
});
