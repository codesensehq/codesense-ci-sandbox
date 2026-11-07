// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Heading } from "@/features/viewer/lib/note-headings";
import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

function h(level: number, text: string, pos: number, slug: string): Heading {
  return { level, text, pos, slug };
}

describe("SW-SN-6 heading-jump store surface", () => {
  beforeEach(() => {
    useViewerStore.setState({
      activeNoteHeadings: [],
      pendingHeadingJump: null,
      activeNoteId: "note-1",
    });
    vi.clearAllMocks();
  });

  it("setActiveNoteHeadings publishes the live heading list (read-model for the modal)", () => {
    const headings = [h(1, "Intro", 0, "intro"), h(2, "Setup", 10, "setup")];
    useViewerStore.getState().setActiveNoteHeadings(headings);
    expect(useViewerStore.getState().activeNoteHeadings).toEqual(headings);
  });

  it("requestHeadingJump sets the pending signal by slug, binds the active note, and bumps the nonce", () => {
    useViewerStore.getState().requestHeadingJump("setup");
    const first = useViewerStore.getState().pendingHeadingJump;
    // noteId binds the jump to the active note so a concurrent switch discards it.
    expect(first).toEqual({ slug: "setup", noteId: "note-1", nonce: 1 });

    // A repeat jump to the SAME slug must still re-fire the panel effect — the
    // nonce makes it a fresh object even when the slug is unchanged.
    useViewerStore.getState().requestHeadingJump("setup");
    const second = useViewerStore.getState().pendingHeadingJump;
    expect(second).toEqual({ slug: "setup", noteId: "note-1", nonce: 2 });
  });

  it("clearHeadingJump resets the signal (the panel calls it after scrolling)", () => {
    useViewerStore.getState().requestHeadingJump("intro");
    useViewerStore.getState().clearHeadingJump();
    expect(useViewerStore.getState().pendingHeadingJump).toBeNull();
  });

  it("the heading read-model + jump signal are NOT persisted (derived view state)", () => {
    const persistSpy = vi.spyOn(useViewerStore.getState(), "persistState");
    useViewerStore.getState().setActiveNoteHeadings([h(1, "X", 0, "x")]);
    useViewerStore.getState().requestHeadingJump("x");
    useViewerStore.getState().clearHeadingJump();
    expect(persistSpy).not.toHaveBeenCalled();
  });
});
