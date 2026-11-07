// SPDX-License-Identifier: MIT
/**
 * Focused unit tests for `useCodeMergeView` (SS-15 AC-14). Mirrors the
 * `use-code-editor.test.ts` shape — most behavioral coverage lives in
 * `CodeMergeView.test.tsx`; this file targets the load-state machine
 * and the imperative handle in isolation.
 */
import { renderHook, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { EditorView } from "@codemirror/view";
import { useCodeMergeView } from "./use-code-merge-view";
import * as cmLoader from "./cm-loader";

const EDITOR_READY_WAIT = { timeout: 5_000 };

beforeEach(() => {
  cmLoader._resetCmLoaderForTests();
  cmLoader._resetCmMergeLoaderForTests();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useCodeMergeView — load-state machine", () => {
  it("starts in 'loading' state when no container is provided", () => {
    const { result } = renderHook(() =>
      useCodeMergeView({
        container: null,
        diskValue: "a",
        localValue: "b",
        theme: "light",
        fontSize: 13,
        gutters: true,
        language: "plain",
      }),
    );
    expect(result.current.loadState).toBe("loading");
    expect(result.current.handle).toBeNull();
    expect(result.current.loadError).toBeNull();
  });

  it("transitions to 'ready' and produces a handle when given a container", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "disk-side",
          localValue: "local-side",
          theme: "dark",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => {
        expect(result.current.loadState).toBe("ready");
      }, EDITOR_READY_WAIT);
      expect(result.current.handle).toBeTruthy();
      expect(result.current.handle?.getLocalValue()).toBe("local-side");
      expect(result.current.handle?.getDiskValue()).toBe("disk-side");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("transitions to 'error' on getCodeMirrorMerge rejection and exposes loadError", async () => {
    const err = new Error("merge boom");
    vi.spyOn(cmLoader, "getCodeMirrorMerge").mockRejectedValueOnce(err);
    const onLoadError = vi.fn();
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "a",
          localValue: "b",
          onLoadError,
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => {
        expect(result.current.loadState).toBe("error");
      });
      expect(result.current.loadError).toBe(err);
      expect(onLoadError).toHaveBeenCalledWith(err);
    } finally {
      document.body.removeChild(container);
    }
  });

  it("retry() re-arms the load after a rejection (mirrors useCodeEditor's retry)", async () => {
    const err = new Error("transient");
    const spy = vi.spyOn(cmLoader, "getCodeMirrorMerge").mockRejectedValueOnce(err);
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "a",
          localValue: "b",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.loadState).toBe("error"));
      spy.mockRestore();
      result.current.retry();
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      expect(result.current.handle).toBeTruthy();
    } finally {
      document.body.removeChild(container);
    }
  });
});

describe("useCodeMergeView — handle shape and behavior", () => {
  it("exposes every documented method on CodeMergeViewHandle", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "x",
          localValue: "y",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      const methods: (keyof typeof h)[] = [
        "getLocalValue",
        "getDiskValue",
        "setLocalValue",
        "setDiskValue",
        "goToNextChunk",
        "goToPreviousChunk",
        "getChunkCount",
      ];
      for (const m of methods) {
        expect(typeof h[m]).toBe("function");
      }
    } finally {
      document.body.removeChild(container);
    }
  });

  it("setLocalValue and setDiskValue update the corresponding side", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "disk-1",
          localValue: "local-1",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      h.setLocalValue("local-2");
      h.setDiskValue("disk-2");
      expect(h.getLocalValue()).toBe("local-2");
      expect(h.getDiskValue()).toBe("disk-2");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("getChunkCount reflects diff count (zero when sides match)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "same\ncontent",
          localValue: "same\ncontent",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      expect(result.current.handle!.getChunkCount()).toBe(0);
    } finally {
      document.body.removeChild(container);
    }
  });

  it("getChunkCount is positive when the two sides differ", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "line1\nDISK\nline3",
          localValue: "line1\nLOCAL\nline3",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      expect(result.current.handle!.getChunkCount()).toBeGreaterThan(0);
    } finally {
      document.body.removeChild(container);
    }
  });

  it("goToNextChunk returns boolean (true when there's a chunk, false otherwise)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "same",
          localValue: "same",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      expect(typeof h.goToNextChunk()).toBe("boolean");
      expect(typeof h.goToPreviousChunk()).toBe("boolean");
    } finally {
      document.body.removeChild(container);
    }
  });
});

describe("useCodeMergeView — handle is null-safe after unmount", () => {
  it("read paths return defaults; write paths no-op", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const { result, unmount } = renderHook(() =>
      useCodeMergeView({
        container,
        diskValue: "x",
        localValue: "y",
        theme: "light",
        fontSize: 13,
        gutters: true,
        language: "plain",
      }),
    );
    await waitFor(() => expect(result.current.handle).toBeTruthy());
    const h = result.current.handle!;
    unmount();
    document.body.removeChild(container);
    expect(() => h.getLocalValue()).not.toThrow();
    expect(h.getLocalValue()).toBe("");
    expect(h.getDiskValue()).toBe("");
    expect(() => h.setLocalValue("ignored")).not.toThrow();
    expect(() => h.setDiskValue("ignored")).not.toThrow();
    expect(h.goToNextChunk()).toBe(false);
    expect(h.goToPreviousChunk()).toBe(false);
    expect(h.getChunkCount()).toBe(0);
  });
});

describe("useCodeMergeView — onSave dedup (mirrors SS-14 D-23)", () => {
  it("rapid Cmd+S keystrokes dedup to a single onSave call", async () => {
    const onSave = vi.fn().mockReturnValue(new Promise(() => {})); // never resolves
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "a",
          localValue: "b",
          onSave,
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const contentDoms = container.querySelectorAll(".cm-content");
      expect(contentDoms.length).toBeGreaterThanOrEqual(1);
      const editable = contentDoms[contentDoms.length - 1] as HTMLElement;
      const ev1 = new KeyboardEvent("keydown", {
        key: "s",
        metaKey: true,
        bubbles: true,
        cancelable: true,
      });
      const ev2 = new KeyboardEvent("keydown", {
        key: "s",
        metaKey: true,
        bubbles: true,
        cancelable: true,
      });
      editable.dispatchEvent(ev1);
      editable.dispatchEvent(ev2);
      await Promise.resolve();
      expect(onSave).toHaveBeenCalledTimes(1);
    } finally {
      document.body.removeChild(container);
    }
  });
});

describe("useCodeMergeView — review mode (SLW-WD-6 AC-2/AC-3)", () => {
  async function mount(opts: {
    review?: boolean;
    mode?: "split" | "inline";
    before?: string;
    after?: string;
    wordWrap?: boolean;
    search?: boolean;
    collapseUnchanged?: boolean;
  }) {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const { result } = renderHook(() =>
      useCodeMergeView({
        container,
        diskValue: opts.before ?? "line1\nOLD\nline3",
        localValue: opts.after ?? "line1\nNEW\nline3",
        theme: "light",
        fontSize: 13,
        gutters: true,
        language: "plain",
        review: opts.review,
        mode: opts.mode,
        wordWrap: opts.wordWrap,
        search: opts.search,
        collapseUnchanged: opts.collapseUnchanged,
      }),
    );
    await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
    return { container, result, cleanup: () => document.body.removeChild(container) };
  }

  it("review split: both panes read-only, no revert controls (AC-2)", async () => {
    const { container, cleanup } = await mount({ review: true, mode: "split" });
    try {
      const contents = container.querySelectorAll(".cm-content");
      expect(contents.length).toBe(2);
      contents.forEach((el) => expect(el.getAttribute("contenteditable")).toBe("false"));
      expect(container.querySelector(".cm-merge-revert")).toBeNull();
    } finally {
      cleanup();
    }
  });

  it("review inline: a single read-only editor, not a MergeView (AC-3)", async () => {
    const { container, result, cleanup } = await mount({ review: true, mode: "inline" });
    try {
      expect(container.querySelector(".cm-mergeView")).toBeNull();
      expect(container.querySelectorAll(".cm-content").length).toBe(1);
      expect(container.querySelector(".cm-content")!.getAttribute("contenteditable")).toBe("false");
      // The inline handle still reports the two sides (after = editor doc,
      // before = captured original) without exposing edit affordances.
      expect(result.current.handle?.getLocalValue()).toBe("line1\nNEW\nline3");
      expect(result.current.handle?.getDiskValue()).toBe("line1\nOLD\nline3");
    } finally {
      cleanup();
    }
  });

  it("default (non-review) split keeps the editable side — regression guard", async () => {
    const { container, result, cleanup } = await mount({});
    try {
      const contents = container.querySelectorAll(".cm-content");
      expect(contents.length).toBe(2);
      // The last pane (b / local) stays editable in the shipped conflict editor.
      expect(contents[contents.length - 1].getAttribute("contenteditable")).toBe("true");
      // Programmatic edits still work on the editable side.
      result.current.handle?.setLocalValue("edited");
      expect(result.current.handle?.getLocalValue()).toBe("edited");
    } finally {
      cleanup();
    }
  });

  it("wordWrap composes EditorView.lineWrapping only when enabled (SLW-DV-6 AC-6)", async () => {
    const off = await mount({ review: true, mode: "inline" });
    try {
      expect(off.container.querySelector(".cm-lineWrapping")).toBeNull();
    } finally {
      off.cleanup();
    }
    const on = await mount({ review: true, mode: "inline", wordWrap: true });
    try {
      expect(on.container.querySelector(".cm-lineWrapping")).not.toBeNull();
    } finally {
      on.cleanup();
    }
  });

  it("search opens a find panel on ⌘F only when enabled (SLW-DV-6 AC-7)", async () => {
    const press = (content: HTMLElement) =>
      content.dispatchEvent(
        new KeyboardEvent("keydown", { key: "f", metaKey: true, bubbles: true, cancelable: true }),
      );

    const off = await mount({ review: true, mode: "inline" });
    try {
      press(off.container.querySelector(".cm-content") as HTMLElement);
      expect(off.container.querySelector(".cm-search")).toBeNull();
    } finally {
      off.cleanup();
    }

    const on = await mount({ review: true, mode: "inline", search: true });
    try {
      press(on.container.querySelector(".cm-content") as HTMLElement);
      expect(on.container.querySelector(".cm-search")).not.toBeNull();
    } finally {
      on.cleanup();
    }
  });

  it("composes wordWrap + search in SPLIT mode too, not just inline (SLW-DV-6 AC-6/AC-7)", async () => {
    // The overlay's non-default mode; both side editors get the shared extensions.
    const { container, cleanup } = await mount({
      review: true,
      mode: "split",
      wordWrap: true,
      search: true,
    });
    try {
      expect(container.querySelector(".cm-mergeView")).not.toBeNull();
      expect(container.querySelector(".cm-lineWrapping")).not.toBeNull();
      const content = container.querySelector(".cm-content") as HTMLElement;
      content.dispatchEvent(
        new KeyboardEvent("keydown", { key: "f", metaKey: true, bubbles: true, cancelable: true }),
      );
      expect(container.querySelector(".cm-search")).not.toBeNull();
    } finally {
      cleanup();
    }
  });

  it("collapses far unchanged regions by default, full context when off (SLW-DV-6 AC-4)", async () => {
    // A single change buried in a long unchanged block → far lines collapse.
    const body = (mid: string) =>
      Array.from({ length: 24 }, (_, i) => (i === 12 ? mid : `line${i}`)).join("\n");
    const collapsed = await mount({
      review: true,
      mode: "inline",
      before: body("OLD"),
      after: body("NEW"),
    });
    try {
      expect(collapsed.container.querySelector(".cm-collapsedLines")).not.toBeNull();
    } finally {
      collapsed.cleanup();
    }
    const expanded = await mount({
      review: true,
      mode: "inline",
      before: body("OLD"),
      after: body("NEW"),
      collapseUnchanged: false,
    });
    try {
      expect(expanded.container.querySelector(".cm-collapsedLines")).toBeNull();
    } finally {
      expanded.cleanup();
    }
  });

  it("highlights changed text within a line, not just the line (SLW-DV-6 AC-5)", async () => {
    const { container, cleanup } = await mount({
      review: true,
      mode: "inline",
      before: "the quick brown fox",
      after: "the quick red fox",
    });
    try {
      // `highlightChanges` wraps the changed token in a `.cm-changedText` span —
      // word-level, distinct from the whole-line `.cm-changedLine` background.
      expect(container.querySelector(".cm-changedText")).not.toBeNull();
    } finally {
      cleanup();
    }
  });

  it("F7 / Shift+F7 move the caret to the next / previous change (SLW-DV-6 AC-3)", async () => {
    // Two separate changed regions (lines 2 and 4) → two chunks to traverse.
    const { container, cleanup } = await mount({
      review: true,
      mode: "inline",
      before: "same\nOLD1\nsame\nOLD2\nsame",
      after: "same\nNEW1\nsame\nNEW2\nsame",
    });
    try {
      const content = container.querySelector(".cm-content") as HTMLElement;
      const view = EditorView.findFromDOM(content)!;
      expect(view).toBeTruthy();
      const press = (shift: boolean) =>
        content.dispatchEvent(
          new KeyboardEvent("keydown", {
            key: "F7",
            shiftKey: shift,
            bubbles: true,
            cancelable: true,
          }),
        );

      view.dispatch({ selection: { anchor: 0 } });
      press(false); // F7 → first change
      const firstChange = view.state.selection.main.head;
      expect(firstChange).toBeGreaterThan(0);

      press(false); // F7 → second change (further down)
      const secondChange = view.state.selection.main.head;
      expect(secondChange).toBeGreaterThan(firstChange);

      press(true); // Shift+F7 → back to the first change
      expect(view.state.selection.main.head).toBeLessThan(secondChange);
    } finally {
      cleanup();
    }
  });
});

describe("useCodeMergeView — live review updates (SLW-DV-5 AC-6b)", () => {
  it("exposes live-follow state only in review mode", async () => {
    const c1 = document.createElement("div");
    document.body.appendChild(c1);
    const c2 = document.createElement("div");
    document.body.appendChild(c2);
    try {
      const { result: nonReview } = renderHook(() =>
        useCodeMergeView({
          container: c1,
          diskValue: "a",
          localValue: "b",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      const { result: review } = renderHook(() =>
        useCodeMergeView({
          container: c2,
          diskValue: "a",
          localValue: "b",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
          review: true,
        }),
      );
      await waitFor(() => expect(nonReview.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      await waitFor(() => expect(review.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      expect(nonReview.current.live).toBeNull();
      expect(review.current.live).not.toBeNull();
      expect(review.current.live?.paused).toBe(false);
    } finally {
      document.body.removeChild(c1);
      document.body.removeChild(c2);
    }
  });

  it("review split: a new after patches side b IN PLACE (no remount)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result, rerender } = renderHook(
        ({ after }) =>
          useCodeMergeView({
            container,
            diskValue: "alpha\nbeta\ngamma",
            localValue: after,
            theme: "light",
            fontSize: 13,
            gutters: true,
            language: "plain",
            review: true,
            mode: "split",
          }),
        { initialProps: { after: "alpha\nbeta\ngamma" } },
      );
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      const bBefore = container.querySelectorAll(".cm-content")[1] as HTMLElement;

      rerender({ after: "alpha\nbeta\ngamma\nDELTA" });

      const bAfter = container.querySelectorAll(".cm-content")[1] as HTMLElement;
      // Same DOM node — the doc was patched, the view was NOT torn down + rebuilt.
      expect(bAfter).toBe(bBefore);
      expect(bAfter.textContent).toContain("DELTA");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("review inline: a new after updates the single editor doc in place", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result, rerender } = renderHook(
        ({ after }) =>
          useCodeMergeView({
            container,
            diskValue: "alpha\nbeta",
            localValue: after,
            theme: "light",
            fontSize: 13,
            gutters: true,
            language: "plain",
            review: true,
            mode: "inline",
          }),
        { initialProps: { after: "alpha\nbeta" } },
      );
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      const elBefore = container.querySelector(".cm-content") as HTMLElement;

      rerender({ after: "alpha\nbeta\nZETA" });

      const elAfter = container.querySelector(".cm-content") as HTMLElement;
      expect(elAfter).toBe(elBefore);
      expect(elAfter.textContent).toContain("ZETA");
      expect(result.current.handle?.getLocalValue()).toBe("alpha\nbeta\nZETA");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("review inline: a new before patches the baseline in place (originalDocChangeEffect)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result, rerender } = renderHook(
        ({ before }) =>
          useCodeMergeView({
            container,
            diskValue: before,
            localValue: "alpha\nbeta\ngamma",
            theme: "light",
            fontSize: 13,
            gutters: true,
            language: "plain",
            review: true,
            mode: "inline",
          }),
        { initialProps: { before: "alpha\nOLD\ngamma" } },
      );
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      const elBefore = container.querySelector(".cm-content") as HTMLElement;

      // Change ONLY the baseline (before) — exercises the inline getOriginalDoc +
      // originalDocChangeEffect path, which no other test drives.
      rerender({ before: "alpha\nNEW\ngamma" });

      const elAfter = container.querySelector(".cm-content") as HTMLElement;
      // Patched in place (no remount) and did not throw; baseline is the new one.
      expect(elAfter).toBe(elBefore);
      expect(result.current.handle?.getDiskValue()).toBe("alpha\nNEW\ngamma");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("review split: an identical after is a no-op (nothing to patch)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result, rerender } = renderHook(
        ({ after }) =>
          useCodeMergeView({
            container,
            diskValue: "x\ny",
            localValue: after,
            theme: "light",
            fontSize: 13,
            gutters: true,
            language: "plain",
            review: true,
            mode: "split",
          }),
        { initialProps: { after: "x\ny" } },
      );
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      const b = container.querySelectorAll(".cm-content")[1] as HTMLElement;
      const html = b.innerHTML;
      rerender({ after: "x\ny" }); // unchanged
      expect((container.querySelectorAll(".cm-content")[1] as HTMLElement).innerHTML).toBe(html);
    } finally {
      document.body.removeChild(container);
    }
  });
});

describe("useCodeMergeView — ignore-whitespace (SLW-DV-8 AC-1)", () => {
  // The `after` differs from `before` ONLY by leading indentation.
  const reindentBefore = "function f() {\nreturn 1;\n}\n";
  const reindentAfter = "function f() {\n    return 1;\n}\n";

  async function chunkCount(opts: {
    after: string;
    diffMode: "line" | "ignoreWhitespace";
  }): Promise<number> {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: reindentBefore,
          localValue: opts.after,
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "javascript",
          review: true,
          mode: "split",
          diffMode: opts.diffMode,
        }),
      );
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      return result.current.handle?.getChunkCount() ?? -1;
    } finally {
      document.body.removeChild(container);
    }
  }

  it("suppresses a whitespace-only chunk in the real MergeView when on", async () => {
    // The override must be attached AND consumed by MergeView — a reindent yields
    // zero change chunks. This is the seam a stub-only test cannot reach.
    expect(await chunkCount({ after: reindentAfter, diffMode: "ignoreWhitespace" })).toBe(0);
  });

  it("shows the whitespace-only chunk when off (control)", async () => {
    expect(await chunkCount({ after: reindentAfter, diffMode: "line" })).toBeGreaterThan(0);
  });

  it("still shows a real edit even with ignore-whitespace on", async () => {
    // Reindented AND 1 -> 2: the content change must survive the override.
    const realEdit = "function f() {\n    return 2;\n}\n";
    expect(await chunkCount({ after: realEdit, diffMode: "ignoreWhitespace" })).toBeGreaterThan(0);
  });
});

describe("useCodeMergeView — structural override (SLW-DV-8b AC-1)", () => {
  const before = "const x = 1\n";
  const after = "const y = 1\n";

  async function structuralChunkCount(
    structuralChanges: readonly { fromA: number; toA: number; fromB: number; toB: number }[],
  ): Promise<number> {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: before,
          localValue: after,
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "typescript",
          review: true,
          mode: "split",
          diffMode: "structural",
          structuralChanges,
        }),
      );
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      return result.current.handle?.getChunkCount() ?? -1;
    } finally {
      document.body.removeChild(container);
    }
  }

  it("renders EXACTLY the cached structural changes, not CM's own diff", async () => {
    // The docs differ (x → y), but an empty structural result forces zero chunks:
    // proof MergeView consumes the `() => cachedChanges` override (AC-3), not its
    // built-in line diff.
    expect(await structuralChunkCount([])).toBe(0);
  });

  it("shows a chunk for a provided structural change", async () => {
    // "x"/"y" sit at offset 6 on each side.
    expect(await structuralChunkCount([{ fromA: 6, toA: 7, fromB: 6, toB: 7 }])).toBeGreaterThan(0);
  });
});

describe("useCodeMergeView — CRLF files (review round 6)", () => {
  it("hands a CRLF file back with its CRLF breaks on both sides", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeMergeView({
          container,
          diskValue: "a\r\ntheirs\r\n",
          localValue: "a\r\nmine\r\n",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
          lineSeparator: "\r\n",
        }),
      );
      await waitFor(() => expect(result.current.loadState).toBe("ready"), EDITOR_READY_WAIT);
      expect(result.current.handle?.getLocalValue()).toBe("a\r\nmine\r\n");
      expect(result.current.handle?.getDiskValue()).toBe("a\r\ntheirs\r\n");
      result.current.handle?.setLocalValue("a\r\nmerged\r\n");
      expect(result.current.handle?.getLocalValue()).toBe("a\r\nmerged\r\n");
    } finally {
      document.body.removeChild(container);
    }
  });
});
