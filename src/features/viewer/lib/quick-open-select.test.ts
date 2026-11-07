// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import { handleQuickOpenFileSelect, type QuickOpenSelectStore } from "./quick-open-select";

function makeStore(): {
  store: QuickOpenSelectStore;
  openFile: ReturnType<typeof vi.fn>;
} {
  const openFile = vi.fn();
  return { openFile, store: { openFile } };
}

describe("handleQuickOpenFileSelect (SW-FX-49)", () => {
  it("passes a private file's own content root to the open and reveal boundary", () => {
    const { store, openFile } = makeStore();
    const root = "/home/users/42/scratch/orgs/acme/plans/content";
    handleQuickOpenFileSelect(store, "acme/plans:dev", "/clone", `${root}/archive/plan.md`);
    expect(openFile).toHaveBeenCalledWith("acme/plans:dev", root, `${root}/archive/plan.md`);
  });
  it("opens the picked file (AC-1, AC-2)", () => {
    const { store, openFile } = makeStore();
    handleQuickOpenFileSelect(store, "org:team", "/repo", "/repo/src/index.ts");
    expect(openFile).toHaveBeenCalledTimes(1);
    expect(openFile).toHaveBeenCalledWith("org:team", "/repo", "/repo/src/index.ts");
  });

  it("opens a story file exactly like any other file — no extra side-effects (AC-1)", () => {
    // The whole SW-FX-49 fix: a story file must NOT boot a workspace on ⌘P.
    // Opening a story-looking path goes through the identical, side-effect-free
    // path as any other file.
    const { store, openFile } = makeStore();
    handleQuickOpenFileSelect(store, "org:team", "/repo", "/repo/_bmad-output/stories/SW-FX-49.md");
    expect(openFile).toHaveBeenCalledTimes(1);
  });

  it("the store-slice type structurally excludes any workspace orchestrator (AC-1 by construction)", () => {
    // The airtight guarantee is a TYPE fact: QuickOpenSelectStore exposes only
    // the peek surface, so the helper cannot reach an orchestrator. This tripwire
    // encodes that: if someone widens the slice to add `openOrFocusStoryWorkspace`
    // (the path of least resistance for re-adding a boot), the @ts-expect-error
    // below stops erroring and the file fails to compile — surfacing the
    // regression at the type level, before any runtime test even runs.
    const slice: QuickOpenSelectStore = { openFile: vi.fn() };
    // @ts-expect-error — the slice must NOT carry a story-workspace orchestrator.
    void slice.openOrFocusStoryWorkspace;
    expect(slice).toBeDefined();
  });

  it("never boots a story workspace even when the passed store CAN (AC-1 runtime tripwire)", () => {
    // Belt-and-suspenders to the structural guard above. The real call site
    // passes the FULL viewer store — which DOES carry `openOrFocusStoryWorkspace`.
    // Model that here (extra spy the helper must never touch) and select a story
    // file: the orchestrator stays untouched. Catches a boot re-added inside the
    // helper via a widened slice — a runtime complement to the compile-time guard.
    const openFile = vi.fn();
    const openOrFocusStoryWorkspace = vi.fn();
    const fullStore = { openFile, openOrFocusStoryWorkspace };
    handleQuickOpenFileSelect(
      fullStore,
      "org:team",
      "/repo",
      "/repo/_bmad-output/stories/SW-FX-49.md",
    );
    expect(openFile).toHaveBeenCalledTimes(1);
    expect(openOrFocusStoryWorkspace).not.toHaveBeenCalled();
  });
});
