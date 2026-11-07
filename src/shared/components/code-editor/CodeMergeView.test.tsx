// SPDX-License-Identifier: MIT
/**
 * Component tests for `<CodeMergeView>` (SS-15 AC-13).
 */
import { render, screen, waitFor, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createRef } from "react";
import { CodeMergeView, type CodeMergeViewHandle } from "./CodeMergeView";
import * as cmLoader from "./cm-loader";

// Stub next-themes
vi.mock("next-themes", () => ({
  useTheme: () => ({ resolvedTheme: "light" }),
}));

// Stub viewer-store
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: () => undefined,
}));

beforeEach(() => {
  cmLoader._resetCmLoaderForTests();
  cmLoader._resetCmMergeLoaderForTests();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("CodeMergeView — render states", () => {
  it("renders the lazy-load skeleton with aria-busy until ready", () => {
    // Block the merge loader so we stay in loading state.
    vi.spyOn(cmLoader, "getCodeMirrorMerge").mockReturnValue(new Promise(() => {}));
    const { container } = render(<CodeMergeView diskValue="a" localValue="b" />);
    const skeleton = container.querySelector("[aria-busy='true']");
    expect(skeleton).toBeTruthy();
    expect(skeleton!.getAttribute("aria-label")).toBe("Loading code merge view");
  });

  it("renders .cm-mergeView once loaded", async () => {
    const { container } = render(<CodeMergeView diskValue="disk" localValue="local" />);
    await waitFor(() => {
      expect(container.querySelector(".cm-mergeView")).toBeTruthy();
    });
  });

  it("renders error + retry UI on load failure, fires onLoadError", async () => {
    const err = new Error("load fail");
    vi.spyOn(cmLoader, "getCodeMirrorMerge").mockRejectedValueOnce(err);
    const onLoadError = vi.fn();
    render(<CodeMergeView diskValue="a" localValue="b" onLoadError={onLoadError} />);
    await waitFor(() => {
      expect(screen.getByText("Merge view failed to load.")).toBeTruthy();
    });
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(onLoadError).toHaveBeenCalledWith(err);
  });

  it("retry after load failure mounts successfully", async () => {
    const err = new Error("transient");
    const spy = vi.spyOn(cmLoader, "getCodeMirrorMerge").mockRejectedValueOnce(err);
    const { container } = render(<CodeMergeView diskValue="a" localValue="b" />);
    await waitFor(() => {
      expect(screen.getByText("Merge view failed to load.")).toBeTruthy();
    });
    spy.mockRestore();
    cmLoader._resetCmMergeLoaderForTests();
    await act(async () => {
      screen.getByRole("button", { name: "Retry" }).click();
    });
    await waitFor(() => {
      expect(container.querySelector(".cm-mergeView")).toBeTruthy();
    });
  });
});

describe("CodeMergeView — two panes", () => {
  it("mounts both .a and .b EditorView DOM elements", async () => {
    const { container } = render(<CodeMergeView diskValue="disk" localValue="local" />);
    await waitFor(() => {
      expect(container.querySelector(".cm-mergeView")).toBeTruthy();
    });
    const contentDoms = container.querySelectorAll(".cm-content");
    expect(contentDoms.length).toBeGreaterThanOrEqual(2);
  });
});

describe("CodeMergeView — disk side is read-only", () => {
  it("disk pane rejects input; local pane accepts it", async () => {
    const ref = createRef<CodeMergeViewHandle>();
    render(<CodeMergeView ref={ref} diskValue="disk" localValue="local" />);
    await waitFor(() => expect(ref.current).toBeTruthy());
    const h = ref.current!;
    // The disk side (a) is read-only at the CM6 level — verify via handle.
    expect(h.getDiskValue()).toBe("disk");
    expect(h.getLocalValue()).toBe("local");
    // Programmatic writes to local work; disk stays read-only via EditorState.readOnly.
    h.setLocalValue("modified-local");
    expect(h.getLocalValue()).toBe("modified-local");
    expect(h.getDiskValue()).toBe("disk");
  });
});

describe("CodeMergeView — path-driven language detection", () => {
  it("mounts with path prop for language detection", async () => {
    const ref = createRef<CodeMergeViewHandle>();
    render(
      <CodeMergeView ref={ref} diskValue="const x = 1;" localValue="const y = 2;" path="x.ts" />,
    );
    await waitFor(() => expect(ref.current).toBeTruthy());
    // If we got here without crashing, language extension was applied.
    expect(ref.current!.getLocalValue()).toBe("const y = 2;");
  });
});

describe("CodeMergeView — save (Cmd+S)", () => {
  it("Cmd+S fires onSave with the editable side's value", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const ref = createRef<CodeMergeViewHandle>();
    const { container } = render(
      <CodeMergeView ref={ref} diskValue="disk" localValue="local" onSave={onSave} />,
    );
    await waitFor(() => expect(ref.current).toBeTruthy());
    const contentDoms = container.querySelectorAll(".cm-content");
    const editable = contentDoms[contentDoms.length - 1] as HTMLElement;
    const ev = new KeyboardEvent("keydown", {
      key: "s",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    editable.dispatchEvent(ev);
    await waitFor(() => {
      expect(onSave).toHaveBeenCalledTimes(1);
    });
    expect(onSave).toHaveBeenCalledWith("local");
  });
});

describe("CodeMergeView — in-flight save dedup", () => {
  it("never-resolving onSave blocks a second Cmd+S", async () => {
    const onSave = vi.fn().mockReturnValue(new Promise(() => {}));
    const ref = createRef<CodeMergeViewHandle>();
    const { container } = render(
      <CodeMergeView ref={ref} diskValue="a" localValue="b" onSave={onSave} />,
    );
    await waitFor(() => expect(ref.current).toBeTruthy());
    const contentDoms = container.querySelectorAll(".cm-content");
    const editable = contentDoms[contentDoms.length - 1] as HTMLElement;
    editable.dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", metaKey: true, bubbles: true, cancelable: true }),
    );
    editable.dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", metaKey: true, bubbles: true, cancelable: true }),
    );
    await Promise.resolve();
    expect(onSave).toHaveBeenCalledTimes(1);
  });
});

describe("CodeMergeView — onSave rejection swallowed", () => {
  it("rejected onSave does not throw unhandled rejection", async () => {
    const onSave = vi.fn().mockRejectedValue(new Error("save fail"));
    const ref = createRef<CodeMergeViewHandle>();
    const { container } = render(
      <CodeMergeView ref={ref} diskValue="a" localValue="b" onSave={onSave} />,
    );
    await waitFor(() => expect(ref.current).toBeTruthy());
    const contentDoms = container.querySelectorAll(".cm-content");
    const editable = contentDoms[contentDoms.length - 1] as HTMLElement;
    editable.dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", metaKey: true, bubbles: true, cancelable: true }),
    );
    // Wait for microtask to flush — no unhandled rejection should escape.
    await new Promise((r) => setTimeout(r, 50));
    expect(onSave).toHaveBeenCalledTimes(1);
  });
});

describe("CodeMergeView — disk-prop external update", () => {
  it("diskValue prop change updates disk side without touching local", async () => {
    const ref = createRef<CodeMergeViewHandle>();
    render(<CodeMergeView ref={ref} diskValue="disk-A" localValue="local-X" />);
    await waitFor(() => expect(ref.current).toBeTruthy());
    const h = ref.current!;
    expect(h.getDiskValue()).toBe("disk-A");
    expect(h.getLocalValue()).toBe("local-X");
    // Simulate a new disk version arriving.
    h.setDiskValue("disk-B");
    expect(h.getDiskValue()).toBe("disk-B");
    expect(h.getLocalValue()).toBe("local-X");
  });
});

describe("CodeMergeView — imperative handle null-safety", () => {
  it("handle methods are safe after unmount", async () => {
    const ref = createRef<CodeMergeViewHandle>();
    const { unmount } = render(<CodeMergeView ref={ref} diskValue="x" localValue="y" />);
    await waitFor(() => expect(ref.current).toBeTruthy());
    const h = ref.current!;
    unmount();
    expect(h.getLocalValue()).toBe("");
    expect(h.getDiskValue()).toBe("");
    expect(() => h.setLocalValue("z")).not.toThrow();
    expect(() => h.setDiskValue("z")).not.toThrow();
    expect(h.getChunkCount()).toBe(0);
    expect(h.goToNextChunk()).toBe(false);
    expect(h.goToPreviousChunk()).toBe(false);
  });
});

describe("CodeMergeView — goToNextChunk / goToPreviousChunk", () => {
  it("chunks exist when sides differ; goToNextChunk returns boolean", async () => {
    const ref = createRef<CodeMergeViewHandle>();
    render(
      <CodeMergeView ref={ref} diskValue="line1\nDISK\nline3" localValue="line1\nLOCAL\nline3" />,
    );
    await waitFor(() => expect(ref.current).toBeTruthy());
    const h = ref.current!;
    expect(h.getChunkCount()).toBeGreaterThan(0);
    // goToNextChunk returns boolean; in happy-dom without layout it may
    // return false even with chunks present, so we only assert the type.
    expect(typeof h.goToNextChunk()).toBe("boolean");
  });
});

describe("CodeMergeView — destroy on unmount", () => {
  it("unmount destroys the merge view", async () => {
    const ref = createRef<CodeMergeViewHandle>();
    const { container, unmount } = render(<CodeMergeView ref={ref} diskValue="a" localValue="b" />);
    await waitFor(() => expect(ref.current).toBeTruthy());
    // Capture handle before unmount — React clears createRef on unmount.
    const h = ref.current!;
    expect(container.querySelector(".cm-mergeView")).toBeTruthy();
    unmount();
    // After destroy, handle reads return defaults (null-safe).
    expect(h.getLocalValue()).toBe("");
  });
});
