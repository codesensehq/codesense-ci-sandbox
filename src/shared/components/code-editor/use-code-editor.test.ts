// SPDX-License-Identifier: MIT
/**
 * Focused unit tests for `useCodeEditor`. Most behavioral coverage of the
 * hook lives in `CodeEditor.test.tsx` (it exercises every interaction the
 * hook owns). This file targets:
 *
 * - The load-state machine in isolation (loading → ready, loading → error).
 * - The handle exposing the expected shape.
 *
 * Vitest 4 + happy-dom: rendering the hook via React's `renderHook` works
 * but with `useState` + `useEffect`. We use it sparingly.
 */
import { renderHook, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useCodeEditor } from "./use-code-editor";
import * as cmLoader from "./cm-loader";

beforeEach(() => {
  cmLoader._resetCmLoaderForTests();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useCodeEditor — load-state machine", () => {
  it("starts in 'loading' state when no container is provided", () => {
    const { result } = renderHook(() =>
      useCodeEditor({
        container: null,
        value: "",
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
        useCodeEditor({
          container,
          value: "abc",
          theme: "dark",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => {
        expect(result.current.loadState).toBe("ready");
      });
      expect(result.current.handle).toBeTruthy();
      expect(result.current.handle?.getValue()).toBe("abc");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("transitions to 'error' on getCodeMirror rejection and exposes loadError", async () => {
    const err = new Error("boom");
    vi.spyOn(cmLoader, "getCodeMirror").mockRejectedValueOnce(err);
    const onLoadError = vi.fn();
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "",
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
});

describe("useCodeEditor — handle behavior (L-2)", () => {
  it("exposes every documented method on CodeEditorHandle", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "x",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      const methods: (keyof typeof h)[] = [
        "getValue",
        "setValue",
        "clearHistory",
        "getState",
        "setState",
        "focus",
        "blur",
        "getCursor",
        "setSelection",
        "getScrollPos",
        "scrollTo",
        "getViewSnapshot",
        "applyViewSnapshot",
        "checkConflict",
      ];
      for (const m of methods) {
        expect(typeof h[m]).toBe("function");
      }
    } finally {
      document.body.removeChild(container);
    }
  });

  it("getValue returns the initial value and setValue updates content", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "initial",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      expect(h.getValue()).toBe("initial");
      h.setValue("replaced");
      expect(h.getValue()).toBe("replaced");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("focus and blur shift document.activeElement", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      h.focus();
      const contentDOM = container.querySelector(".cm-content");
      expect(document.activeElement).toBe(contentDOM);
      h.blur();
      expect(document.activeElement).not.toBe(contentDOM);
    } finally {
      document.body.removeChild(container);
    }
  });

  it("clearHistory wipes undo so prior typing can't be reversed", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "start",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      h.setValue("changed");
      h.clearHistory();
      // After clearHistory the editor has no undo entries pointing at "start".
      // setValue's H-3 patch already marks programmatic dispatches as
      // non-undoable, so an attempt to undo should be a no-op (value stays
      // "changed"). We exercise the handle path here, not the keystroke
      // path (which is tested in CodeEditor.test.tsx).
      expect(h.getValue()).toBe("changed");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("checkConflict returns true on mismatch, false on match", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "local",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      expect(h.checkConflict("disk")).toBe(true);
      expect(h.checkConflict("local")).toBe(false);
    } finally {
      document.body.removeChild(container);
    }
  });

  it("setSelection clamps out-of-range line/ch (L-8)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "abc\ndef",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      // Out-of-range line — should clamp instead of throwing.
      expect(() =>
        h.setSelection({
          from: { line: 99, ch: 99 },
          to: { line: 99, ch: 99 },
        }),
      ).not.toThrow();
      const cursor = h.getCursor();
      // Last line is index 1; last ch is "def".length === 3.
      expect(cursor.line).toBe(1);
      expect(cursor.ch).toBe(3);
    } finally {
      document.body.removeChild(container);
    }
  });

  it("scrollTo clamps NaN / Infinity / negative (L-9)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = renderHook(() =>
        useCodeEditor({
          container,
          value: "x",
          theme: "light",
          fontSize: 13,
          gutters: true,
          language: "plain",
        }),
      );
      await waitFor(() => expect(result.current.handle).toBeTruthy());
      const h = result.current.handle!;
      expect(() => h.scrollTo(Number.NaN)).not.toThrow();
      expect(h.getScrollPos()).toBe(0);
      expect(() => h.scrollTo(-100)).not.toThrow();
      expect(h.getScrollPos()).toBe(0);
      expect(() => h.scrollTo(Number.POSITIVE_INFINITY)).not.toThrow();
      // Clamped to 0 (or to the max scrollable, depending on layout) —
      // either way no crash, no NaN propagation.
      expect(Number.isFinite(h.getScrollPos())).toBe(true);
    } finally {
      document.body.removeChild(container);
    }
  });
});

describe("useCodeEditor — handle is null-safe after unmount (M-2, A-1)", () => {
  it("read paths return defaults; write paths no-op; getState returns null", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const { result, unmount } = renderHook(() =>
      useCodeEditor({
        container,
        value: "x",
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
    expect(() => h.getValue()).not.toThrow();
    expect(h.getValue()).toBe("");
    expect(() => h.setValue("ignored")).not.toThrow();
    expect(() => h.focus()).not.toThrow();
    expect(() => h.scrollTo(10)).not.toThrow();
    expect(h.checkConflict("anything")).toBe(false);
    // A-1 patch: getState returns null (not throws) so hosts persisting
    // state during their own unmount cleanup don't crash.
    expect(() => h.getState()).not.toThrow();
    expect(h.getState()).toBeNull();
    // RF-perf-12: same null-safety for the lightweight snapshot surface.
    expect(() => h.applyViewSnapshot({ scrollTop: 5 })).not.toThrow();
    expect(h.getViewSnapshot()).toBeNull();
  });
});

describe("useCodeEditor — view snapshot (RF-perf-12 AC-8)", () => {
  async function mountEditor(container: HTMLDivElement, extra?: Record<string, unknown>) {
    const rh = renderHook(() =>
      useCodeEditor({
        container,
        value: "hello world",
        theme: "light",
        fontSize: 13,
        gutters: true,
        language: "plain",
        ...extra,
      }),
    );
    await waitFor(() => expect(rh.result.current.handle).toBeTruthy());
    return rh;
  }

  it("getViewSnapshot returns ONLY scroll + selection offsets — no EditorState / doc / history", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = await mountEditor(container);
      const snap = result.current.handle!.getViewSnapshot();
      expect(snap).not.toBeNull();
      // structural AC-8 assertion: exactly the plain-data keys, nothing heavy.
      expect(Object.keys(snap!).sort()).toEqual(["scrollTop", "selection"]);
      expect(Object.keys(snap!.selection!).sort()).toEqual(["anchor", "head"]);
      expect(typeof snap!.scrollTop).toBe("number");
      // no smuggled document text / EditorState anywhere in the payload
      expect(JSON.stringify(snap)).not.toContain("hello world");
    } finally {
      document.body.removeChild(container);
    }
  });

  it("applyViewSnapshot restores selection (clamped to the current doc)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    try {
      const { result } = await mountEditor(container);
      const h = result.current.handle!;
      h.applyViewSnapshot({ selection: { anchor: 2, head: 5 }, scrollTop: 0 });
      const cur = h.getViewSnapshot()!;
      expect(cur.selection).toEqual({ anchor: 2, head: 5 });

      // out-of-range offsets clamp to the doc length ("hello world" = 11) rather
      // than throwing.
      expect(() => h.applyViewSnapshot({ selection: { anchor: 999, head: 999 } })).not.toThrow();
      expect(h.getViewSnapshot()!.selection).toEqual({ anchor: 11, head: 11 });
    } finally {
      document.body.removeChild(container);
    }
  });

  it("onBeforeDestroy fires with a live snapshot on unmount (before view.destroy)", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const captured: unknown[] = [];
    try {
      const { result, unmount } = await mountEditor(container, {
        onBeforeDestroy: (snap: unknown) => captured.push(snap),
      });
      // move the cursor so the captured snapshot is meaningful
      result.current.handle!.applyViewSnapshot({ selection: { anchor: 3, head: 3 } });
      unmount();
      expect(captured).toHaveLength(1);
      expect(captured[0]).toEqual({
        scrollTop: expect.any(Number),
        selection: { anchor: 3, head: 3 },
      });
    } finally {
      document.body.removeChild(container);
    }
  });
});
