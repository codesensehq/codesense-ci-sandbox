// SPDX-License-Identifier: MIT
import { StrictMode } from "react";
import { vi, describe, it, expect, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

const readFile = vi.fn<(p: string) => Promise<string>>();
const writeContentFile = vi.fn<(p: string, c: string) => Promise<void>>().mockResolvedValue();
vi.mock("@/tauri/commands", () => ({
  readFile: (p: string) => readFile(p),
  writeContentFile: (p: string, c: string) => writeContentFile(p, c),
}));

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));

import { toast } from "sonner";
import { useAttachCanvasToStory } from "./use-attach-canvas-to-story";
import { buildStoryRef } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

const toastFn = vi.mocked(toast);

const STORY_MD = `---\ntype: story\nstory_id: SW-SC-12\n---\n\n# Story\n`;
const CANVAS = "/ctx/epic/design.excalidraw";

function story(fileName: string, absolutePath: string) {
  return buildStoryRef(fileName, { absolutePath });
}

beforeEach(() => {
  vi.clearAllMocks();
  readFile.mockReset();
  writeContentFile.mockResolvedValue();
});

describe("useAttachCanvasToStory (SW-SC-12)", () => {
  it("attaches immediately to a single candidate story and writes supporting_docs", async () => {
    readFile.mockResolvedValue(STORY_MD);
    const s = story("SW-SC-12.md", "/ctx/epic/SW-SC-12.md");

    const { result } = renderHook(() => useAttachCanvasToStory());
    act(() => result.current.open(CANVAS, [s]));

    await waitFor(() => expect(writeContentFile).toHaveBeenCalledTimes(1));
    const [path, text] = writeContentFile.mock.calls[0];
    expect(path).toBe("/ctx/epic/SW-SC-12.md");
    expect(text).toContain("supporting_docs:\n  - ./design.excalidraw");
    expect(toastFn.success).toHaveBeenCalledWith("Attached to SW-SC-12");
    // Single-story attach never opens the picker.
    expect(result.current.picker).toBeNull();
  });

  it("is idempotent — a canvas already attached writes nothing and toasts 'Already attached'", async () => {
    // Story already lists the canvas → upsert reports changed === false.
    readFile.mockResolvedValue(
      `---\ntype: story\nsupporting_docs:\n  - ./design.excalidraw\n---\n\n# Story\n`,
    );
    const s = story("SW-SC-12.md", "/ctx/epic/SW-SC-12.md");

    const { result } = renderHook(() => useAttachCanvasToStory());
    act(() => result.current.open(CANVAS, [s]));

    await waitFor(() => expect(toastFn).toHaveBeenCalledWith("Already attached to SW-SC-12"));
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("opens the picker for 2+ candidates without writing", async () => {
    const a = story("A.md", "/ctx/epic/A.md");
    const b = story("B.md", "/ctx/epic/B.md");

    const { result } = renderHook(() => useAttachCanvasToStory());
    act(() => result.current.open(CANVAS, [a, b]));

    expect(result.current.picker).toEqual({ canvasPath: CANVAS, stories: [a, b] });
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("is a no-op for an empty candidate set (0 stories)", () => {
    const { result } = renderHook(() => useAttachCanvasToStory());
    act(() => result.current.open(CANVAS, []));

    expect(result.current.picker).toBeNull();
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("selectFromPicker attaches the picked story and clears the picker", async () => {
    readFile.mockResolvedValue(STORY_MD);
    const a = story("A.md", "/ctx/epic/A.md");
    const b = story("B.md", "/ctx/epic/B.md");

    const { result } = renderHook(() => useAttachCanvasToStory());
    act(() => result.current.open(CANVAS, [a, b]));
    act(() => result.current.selectFromPicker("/ctx/epic/B.md"));

    await waitFor(() => expect(writeContentFile).toHaveBeenCalledTimes(1));
    expect(writeContentFile.mock.calls[0][0]).toBe("/ctx/epic/B.md");
    expect(toastFn.success).toHaveBeenCalledWith("Attached to B");
    expect(result.current.picker).toBeNull();
  });

  it("writes exactly once under StrictMode (no side effect in the state updater)", async () => {
    readFile.mockResolvedValue(STORY_MD);
    const a = story("A.md", "/ctx/epic/A.md");
    const b = story("B.md", "/ctx/epic/B.md");

    const { result } = renderHook(() => useAttachCanvasToStory(), { wrapper: StrictMode });
    act(() => result.current.open(CANVAS, [a, b]));
    act(() => result.current.selectFromPicker("/ctx/epic/B.md"));

    await waitFor(() => expect(writeContentFile).toHaveBeenCalledTimes(1));
    // StrictMode double-invokes state updaters in dev; a side effect placed
    // inside one would fire the write twice. Pin it to once.
    expect(writeContentFile).toHaveBeenCalledTimes(1);
  });

  it("toasts an error when the write flow throws", async () => {
    readFile.mockRejectedValue(new Error("disk gone"));
    const s = story("SW-SC-12.md", "/ctx/epic/SW-SC-12.md");

    const { result } = renderHook(() => useAttachCanvasToStory());
    act(() => result.current.open(CANVAS, [s]));

    await waitFor(() =>
      expect(toastFn.error).toHaveBeenCalledWith("Couldn't attach the canvas to that story."),
    );
    expect(writeContentFile).not.toHaveBeenCalled();
  });
});
