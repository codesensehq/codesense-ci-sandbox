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
import { useAttachDocToNode } from "./use-attach-doc-to-node";

const toastFn = vi.mocked(toast);

const STORY_MD = `---\ntype: story\nstory_id: SW-GS-8\n---\n\n# Story\n`;
const DOC = "/ctx/team/content/docs/loose-note.md";
const NODE_MD = "/ctx/team/content/sync/SW-GS-8-node-level-publish.md";

beforeEach(() => {
  vi.clearAllMocks();
  readFile.mockReset();
  writeContentFile.mockResolvedValue();
});

describe("useAttachDocToNode (SW-GS-11)", () => {
  it("open() sets the picker for the given doc without writing", () => {
    const { result } = renderHook(() => useAttachDocToNode());
    act(() => result.current.open(DOC));

    expect(result.current.picker).toEqual({ docPath: DOC });
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("selectNode() writes the doc into the chosen node's supporting_docs and clears the picker", async () => {
    readFile.mockResolvedValue(STORY_MD);
    const { result } = renderHook(() => useAttachDocToNode());

    act(() => result.current.open(DOC));
    act(() => result.current.selectNode(NODE_MD, "SW-GS-8"));

    await waitFor(() => expect(writeContentFile).toHaveBeenCalledTimes(1));
    const [path, text] = writeContentFile.mock.calls[0];
    expect(path).toBe(NODE_MD);
    // Doc recorded relative to the node dir — a `../` climb from sync/ to docs/.
    expect(text).toContain("supporting_docs:\n  - ../docs/loose-note.md");
    expect(toastFn.success).toHaveBeenCalledWith("Attached to SW-GS-8");
    expect(result.current.picker).toBeNull();
  });

  it("is idempotent — attaching an already-listed doc writes nothing and toasts 'Already attached'", async () => {
    readFile.mockResolvedValue(
      `---\ntype: story\nsupporting_docs:\n  - ../docs/loose-note.md\n---\n\n# Story\n`,
    );
    const { result } = renderHook(() => useAttachDocToNode());

    act(() => result.current.open(DOC));
    act(() => result.current.selectNode(NODE_MD, "SW-GS-8"));

    await waitFor(() => expect(toastFn).toHaveBeenCalledWith("Already attached to SW-GS-8"));
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("toasts an error when the write flow throws", async () => {
    readFile.mockRejectedValue(new Error("disk gone"));
    const { result } = renderHook(() => useAttachDocToNode());

    act(() => result.current.open(DOC));
    act(() => result.current.selectNode(NODE_MD, "SW-GS-8"));

    await waitFor(() => expect(toastFn.error).toHaveBeenCalledWith("Couldn't attach that doc."));
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("selectNode() is a no-op when no picker is open", () => {
    const { result } = renderHook(() => useAttachDocToNode());
    act(() => result.current.selectNode(NODE_MD, "SW-GS-8"));
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("closePicker() dismisses without writing", () => {
    const { result } = renderHook(() => useAttachDocToNode());
    act(() => result.current.open(DOC));
    act(() => result.current.closePicker());

    expect(result.current.picker).toBeNull();
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("writes exactly once under StrictMode (no side effect in the state updater)", async () => {
    readFile.mockResolvedValue(STORY_MD);
    const { result } = renderHook(() => useAttachDocToNode(), { wrapper: StrictMode });

    act(() => result.current.open(DOC));
    act(() => result.current.selectNode(NODE_MD, "SW-GS-8"));

    await waitFor(() => expect(writeContentFile).toHaveBeenCalledTimes(1));
    expect(writeContentFile).toHaveBeenCalledTimes(1);
  });
});
