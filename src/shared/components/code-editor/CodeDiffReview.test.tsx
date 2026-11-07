// SPDX-License-Identifier: MIT
/**
 * Component tests for `<CodeDiffReview>` (SLW-WD-6) — the read-only,
 * syntax-highlighted source code diff. Asserts the review wiring (read-only
 * both sides, split vs inline construction, graceful load/error), NOT CM6
 * internals.
 */
import * as React from "react";
import { render, screen, waitFor, act, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach, onTestFinished } from "vitest";
import { CodeDiffReview } from "./CodeDiffReview";
import * as cmLoader from "./cm-loader";

// Stub next-themes + viewer-store (mirrors CodeMergeView.test.tsx).
vi.mock("next-themes", () => ({
  useTheme: () => ({ resolvedTheme: "light" }),
}));
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: () => undefined,
}));

// SLW-DV-8b: mock the off-thread structural hook so the indicator + fallback are
// deterministic (and the `?worker` chain is never exercised in jsdom). Default =
// idle (line mode) so the existing suites are unaffected.
const { mockUseStructuralDiff } = vi.hoisted(() => ({ mockUseStructuralDiff: vi.fn() }));
vi.mock("./use-structural-diff", () => ({ useStructuralDiff: mockUseStructuralDiff }));

beforeEach(() => {
  cmLoader._resetCmLoaderForTests();
  cmLoader._resetCmMergeLoaderForTests();
  mockUseStructuralDiff.mockReturnValue({ changes: null, status: "idle" });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("CodeDiffReview — load states (AC-6)", () => {
  it("renders the lazy-load skeleton with aria-busy until ready", () => {
    vi.spyOn(cmLoader, "getCodeMirrorMerge").mockReturnValue(new Promise(() => {}));
    const { container } = render(<CodeDiffReview before="a" after="b" path="x.ts" />);
    const skeleton = container.querySelector("[aria-busy='true']");
    expect(skeleton).toBeTruthy();
    expect(skeleton!.getAttribute("aria-label")).toBe("Loading diff…");
  });

  it("renders an error + Retry inline on chunk-load failure, fires onLoadError (no crash)", async () => {
    const err = new Error("chunk fail");
    vi.spyOn(cmLoader, "getCodeMirrorMerge").mockRejectedValueOnce(err);
    const onLoadError = vi.fn();
    render(<CodeDiffReview before="a" after="b" path="x.ts" onLoadError={onLoadError} />);
    await waitFor(() => {
      expect(screen.getByText("Couldn't load the diff view.")).toBeTruthy();
    });
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
    expect(onLoadError).toHaveBeenCalledWith(err);
  });

  it("retry after a failure mounts successfully", async () => {
    const spy = vi
      .spyOn(cmLoader, "getCodeMirrorMerge")
      .mockRejectedValueOnce(new Error("transient"));
    const { container } = render(<CodeDiffReview before="a" after="b" path="x.ts" />);
    await waitFor(() => expect(screen.getByText("Couldn't load the diff view.")).toBeTruthy());
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

describe("CodeDiffReview — read-only review (AC-2)", () => {
  it("split: mounts a MergeView with BOTH sides read-only and no revert controls", async () => {
    const { container } = render(
      <CodeDiffReview before={"line1\nOLD\nline3"} after={"line1\nNEW\nline3"} path="x.ts" />,
    );
    await waitFor(() => expect(container.querySelector(".cm-mergeView")).toBeTruthy());
    const contents = container.querySelectorAll(".cm-content");
    expect(contents.length).toBe(2);
    // Both panes are read-only — no editable side (unlike CodeMergeView).
    contents.forEach((el) => {
      expect(el.getAttribute("contenteditable")).toBe("false");
    });
    // Review mode drops the a→b revert arrows.
    expect(container.querySelector(".cm-merge-revert")).toBeNull();
  });
});

describe("CodeDiffReview — split vs inline (AC-3)", () => {
  it("split (default) builds a two-pane MergeView", async () => {
    const { container } = render(<CodeDiffReview before="a" after="b" path="x.ts" mode="split" />);
    await waitFor(() => expect(container.querySelector(".cm-mergeView")).toBeTruthy());
    expect(container.querySelectorAll(".cm-content").length).toBe(2);
  });

  it("SLW-WD-7 AC-4: a real split MergeView consumer mounts inside a scrollable frame", async () => {
    const { container } = render(<CodeDiffReview before="a" after="b" path="x.ts" mode="split" />);
    await waitFor(() => expect(container.querySelector(".cm-mergeView")).toBeTruthy());
    // Ties the frame's scroll contract to a REAL mounted split MergeView (the
    // frame carries the contract unconditionally — the `merge-view-frame`
    // suite proves that across modes/states; this proves the actual split
    // consumer inherits it). The overgrown side-by-side view is no longer
    // clipped (was overflow:hidden before WD-7).
    const frame = screen.getByTestId("code-diff-review");
    expect(frame.style.overflowY).toBe("auto");
    expect(frame.style.overflowX).toBe("hidden");
  });

  it("inline builds a single-editor unified view (no MergeView, one content pane)", async () => {
    const { container } = render(
      <CodeDiffReview before={"a\nb\nc"} after={"a\nX\nc"} path="x.ts" mode="inline" />,
    );
    await waitFor(() => expect(container.querySelector(".cm-content")).toBeTruthy());
    // Unified inline is a single EditorView — not a split MergeView.
    expect(container.querySelector(".cm-mergeView")).toBeNull();
    expect(container.querySelectorAll(".cm-content").length).toBe(1);
    // Still read-only in review.
    expect(container.querySelector(".cm-content")!.getAttribute("contenteditable")).toBe("false");
  });
});

describe("CodeDiffReview — unknown language (AC-6)", () => {
  it("an unknown extension still renders the diff (plain, no crash)", async () => {
    const { container } = render(
      <CodeDiffReview before="a" after="b" path="x.unknownext" mode="split" />,
    );
    await waitFor(() => expect(container.querySelector(".cm-mergeView")).toBeTruthy());
    expect(container.querySelectorAll(".cm-content").length).toBe(2);
  });
});

describe("CodeDiffReview — structural mode indicator + fallback (SLW-DV-8b AC-1/AC-5)", () => {
  const props = { before: "const a = 1", after: "const a = 2", path: "x.ts" } as const;

  it("no structural indicator in line mode (default)", () => {
    render(<CodeDiffReview {...props} />);
    expect(screen.queryByTestId("code-diff-structural-status")).toBeNull();
  });

  it("shows a 'computing' indicator while the structural diff runs", () => {
    mockUseStructuralDiff.mockReturnValue({ changes: null, status: "computing" });
    render(<CodeDiffReview {...props} diffMode="structural" />);
    expect(screen.getByTestId("code-diff-structural-status")).toHaveTextContent(/computing/i);
  });

  it("states 'Structural diff' when ready", () => {
    mockUseStructuralDiff.mockReturnValue({ changes: [], status: "ready" });
    render(<CodeDiffReview {...props} diffMode="structural" />);
    expect(screen.getByTestId("code-diff-structural-status")).toHaveTextContent("Structural diff");
  });

  it("falls back honestly for an unsupported language", () => {
    mockUseStructuralDiff.mockReturnValue({ changes: null, status: "unsupported" });
    render(<CodeDiffReview {...props} diffMode="structural" />);
    expect(screen.getByTestId("code-diff-structural-status")).toHaveTextContent(
      /unavailable for this language/i,
    );
  });

  it("falls back honestly when the worker fails", () => {
    mockUseStructuralDiff.mockReturnValue({ changes: null, status: "failed" });
    render(<CodeDiffReview {...props} diffMode="structural" />);
    expect(screen.getByTestId("code-diff-structural-status")).toHaveTextContent(
      /unavailable — showing line diff/i,
    );
  });
});

describe("CodeDiffReview — structural override reaches the diff (SLW-DV-8b AC-1/AC-4)", () => {
  // before/after DIFFER, so CM's OWN line diff would mark a change. These prove the
  // `effectiveMode`/`structuralChanges` composition is actually forwarded into the
  // MergeView — the indicator tests above pass regardless of that forwarding.
  const before = "const x = 1\n";
  const after = "const y = 1\n";
  const changedLines = (root: ParentNode) => root.querySelectorAll(".cm-changedLine").length;

  it("empty ready result → the override wins over CM's line diff (zero change chunks)", async () => {
    mockUseStructuralDiff.mockReturnValue({ changes: [], status: "ready" });
    const { container } = render(
      <CodeDiffReview
        before={before}
        after={after}
        path="x.ts"
        mode="split"
        diffMode="structural"
      />,
    );
    await waitFor(() => expect(container.querySelector(".cm-mergeView")).toBeTruthy());
    // Docs differ but the structural result is empty → no change is shown. If the
    // forwarding broke (effectiveMode fell back to line), CM's diff paints x→y
    // synchronously at mount, so this stays failing until timeout. `waitFor` also
    // absorbs any late decoration paint before settling.
    await waitFor(() => expect(changedLines(container)).toBe(0));
  });

  it("a real ready structural change → the MergeView marks it (change forwarded)", async () => {
    // "x"/"y" sit at offset 6 on each side.
    mockUseStructuralDiff.mockReturnValue({
      changes: [{ fromA: 6, toA: 7, fromB: 6, toB: 7 }],
      status: "ready",
    });
    const { container } = render(
      <CodeDiffReview
        before={before}
        after={after}
        path="x.ts"
        mode="split"
        diffMode="structural"
      />,
    );
    // A structural override's change decorations paint a tick AFTER `.cm-mergeView`
    // mounts, so the count must be awaited — a synchronous read races to 0.
    await waitFor(() => expect(changedLines(container)).toBeGreaterThan(0));
  });

  it("while computing it shows CM's line diff, not a stale/empty structural override", async () => {
    mockUseStructuralDiff.mockReturnValue({ changes: null, status: "computing" });
    const { container } = render(
      <CodeDiffReview
        before={before}
        after={after}
        path="x.ts"
        mode="split"
        diffMode="structural"
      />,
    );
    await waitFor(() => expect(container.querySelector(".cm-mergeView")).toBeTruthy());
    // effectiveMode falls back to "line" during the recompute → CM marks x→y.
    await waitFor(() => expect(changedLines(container)).toBeGreaterThan(0));
  });

  it("a live edit re-resolves the view (structural → recompute) without a thrown effect", async () => {
    // Settled structural (empty result) shows zero chunks for the differing docs.
    mockUseStructuralDiff.mockReturnValue({ changes: [], status: "ready" });
    const { container, rerender } = render(
      <CodeDiffReview
        before={before}
        after={after}
        path="x.ts"
        mode="split"
        diffMode="structural"
      />,
    );
    await waitFor(() => expect(container.querySelector(".cm-mergeView")).toBeTruthy());
    expect(changedLines(container)).toBe(0);

    // A live edit lands: the hook reports `computing` for the new content, so the
    // view re-resolves to CM's line diff (AC-4) rather than a stale override — and
    // it does so cleanly, with no thrown effect (the waitFor below would surface one).
    mockUseStructuralDiff.mockReturnValue({ changes: null, status: "computing" });
    await act(async () => {
      rerender(
        <CodeDiffReview
          before={before}
          after="const z = 1\n"
          path="x.ts"
          mode="split"
          diffMode="structural"
        />,
      );
    });
    await waitFor(() => expect(changedLines(container)).toBeGreaterThan(0));
  });
});

describe("CodeDiffReview — find-in-diff (SLW-DV-6 AC-7)", () => {
  it("⌘F opens the search panel — CodeDiffReview enables search on the real hook", async () => {
    // Guards the `search: true` wiring: without it the whole suite would still
    // pass (the hook test passes `search` directly), so assert it here through
    // the component that hardcodes the flag.
    const { container } = render(
      <CodeDiffReview before={"alpha\nbeta"} after={"alpha\ngamma"} path="x.ts" mode="inline" />,
    );
    await waitFor(() => expect(container.querySelector(".cm-content")).toBeTruthy());
    const content = container.querySelector(".cm-content") as HTMLElement;
    content.dispatchEvent(
      new KeyboardEvent("keydown", { key: "f", metaKey: true, bubbles: true, cancelable: true }),
    );
    expect(container.querySelector(".cm-search")).not.toBeNull();
  });
});

describe("CodeDiffReview — change navigator", () => {
  const before = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].join("\n") + "\n";
  // Two separated edits → two chunks.
  const after = ["a", "B", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";

  for (const mode of ["split", "inline"] as const) {
    it(`${mode}: reports the change count and steps through it`, async () => {
      const onChunkPosition = vi.fn();
      const navRef = { current: null as import("./CodeDiffReview").DiffNavigator | null };
      render(
        <CodeDiffReview
          before={before}
          after={after}
          path="x.txt"
          mode={mode}
          collapseUnchanged={false}
          navRef={navRef}
          onChunkPosition={onChunkPosition}
        />,
      );
      await waitFor(() => expect(navRef.current).not.toBeNull());
      await waitFor(() =>
        expect(onChunkPosition).toHaveBeenCalledWith(expect.objectContaining({ count: 2 })),
      );
      act(() => navRef.current!.next());
      act(() => navRef.current!.next());
      expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 1, count: 2 });
      act(() => navRef.current!.prev());
      expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 0, count: 2 });
    });
  }

  for (const mode of ["split", "inline"] as const) {
    it(`${mode}: the first "next" lands on a change at the very top instead of skipping it`, async () => {
      const top = ["A", "b", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";
      const onChunkPosition = vi.fn();
      const navRef = { current: null as import("./CodeDiffReview").DiffNavigator | null };
      render(
        <CodeDiffReview
          before={before}
          after={top}
          path="x.txt"
          mode={mode}
          collapseUnchanged={false}
          navRef={navRef}
          onChunkPosition={onChunkPosition}
        />,
      );
      await waitFor(() => expect(navRef.current).not.toBeNull());
      act(() => navRef.current!.next());
      expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 0, count: 2 });
      act(() => navRef.current!.next());
      expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 1, count: 2 });
    });

    it(`${mode}: the first "next" scrolls a top change back into view after the reader scrolled away`, async () => {
      const { EditorView } = await import("@codemirror/view");
      const dispatch = vi.spyOn(EditorView.prototype, "dispatch");
      try {
        const top = ["A", "b", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";
        const navRef = { current: null as import("./CodeDiffReview").DiffNavigator | null };
        render(
          <CodeDiffReview
            before={before}
            after={top}
            path="x.txt"
            mode={mode}
            collapseUnchanged={false}
            navRef={navRef}
          />,
        );
        await waitFor(() => expect(navRef.current).not.toBeNull());
        dispatch.mockClear();
        act(() => navRef.current!.next());
        const scrolled = dispatch.mock.calls.some((args) =>
          args.some(
            (spec) =>
              !!spec &&
              typeof spec === "object" &&
              "scrollIntoView" in spec &&
              (spec as { scrollIntoView?: boolean }).scrollIntoView === true,
          ),
        );
        expect(scrolled).toBe(true);
      } finally {
        dispatch.mockRestore();
      }
    });

    it(`${mode}: reports the worktree line of the change being read`, async () => {
      const navRef = { current: null as import("./CodeDiffReview").DiffNavigator | null };
      render(
        <CodeDiffReview
          before={before}
          after={after}
          path="x.txt"
          mode={mode}
          collapseUnchanged={false}
          navRef={navRef}
        />,
      );
      await waitFor(() => expect(navRef.current).not.toBeNull());
      // Before the reader moves, the first change is "where they are".
      expect(navRef.current!.currentLine()).toBe(2);
      act(() => navRef.current!.next());
      act(() => navRef.current!.next());
      expect(navRef.current!.currentLine()).toBe(9);
    });
  }
});

describe("CodeDiffReview — navigator edges", () => {
  const before = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].join("\n") + "\n";
  const two = ["a", "B", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";
  const one = ["a", "b", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";
  type Nav = import("./CodeDiffReview").DiffNavigator;

  it("the first report of a diff with no change at the top has no current change", async () => {
    const onChunkPosition = vi.fn();
    render(
      <CodeDiffReview
        before={before}
        after={two}
        path="x.txt"
        collapseUnchanged={false}
        navRef={{ current: null }}
        onChunkPosition={onChunkPosition}
      />,
    );
    await waitFor(() => expect(onChunkPosition).toHaveBeenCalled());
    expect(onChunkPosition.mock.calls[0][0]).toEqual({ index: null, count: 2 });
  });

  it("with a single change, n and p keep bringing it back into view", async () => {
    const { EditorView } = await import("@codemirror/view");
    const dispatch = vi.spyOn(EditorView.prototype, "dispatch");
    try {
      const onChunkPosition = vi.fn();
      const navRef = { current: null as Nav | null };
      render(
        <CodeDiffReview
          before={before}
          after={one}
          path="x.txt"
          collapseUnchanged={false}
          navRef={navRef}
          onChunkPosition={onChunkPosition}
        />,
      );
      await waitFor(() => expect(navRef.current).not.toBeNull());
      act(() => navRef.current!.next());
      expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 0, count: 1 });
      const reveals = () =>
        dispatch.mock.calls.filter((args) =>
          args.some(
            (spec) =>
              !!spec &&
              typeof spec === "object" &&
              (spec as { userEvent?: string }).userEvent === "select.reveal",
          ),
        ).length;
      const before2 = reveals();
      act(() => navRef.current!.next());
      act(() => navRef.current!.prev());
      expect(reveals()).toBe(before2 + 2);
      expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 0, count: 1 });
    } finally {
      dispatch.mockRestore();
    }
  });

  it("a live edit that adds a change re-reports the count", async () => {
    // jsdom has no layout: the live update's scroll anchor reads none.
    const { EditorView } = await import("@codemirror/view");
    const coords = vi.spyOn(EditorView.prototype, "posAtCoords").mockReturnValue(null);
    onTestFinished(() => coords.mockRestore());
    const onChunkPosition = vi.fn();
    const props = {
      before,
      path: "x.txt",
      collapseUnchanged: false,
      navRef: { current: null as Nav | null },
      onChunkPosition,
    };
    const { rerender } = render(<CodeDiffReview {...props} after={one} />);
    await waitFor(() =>
      expect(onChunkPosition).toHaveBeenLastCalledWith(expect.objectContaining({ count: 1 })),
    );
    rerender(<CodeDiffReview {...props} after={two} />);
    await waitFor(() =>
      expect(onChunkPosition).toHaveBeenLastCalledWith(expect.objectContaining({ count: 2 })),
    );
  });

  for (const mode of ["split", "inline"] as const) {
    it(`${mode}: "previous" from the start of the line just after a change lands on that change`, async () => {
      const onChunkPosition = vi.fn();
      const navRef = { current: null as Nav | null };
      const { getByTestId } = render(
        <CodeDiffReview
          before={before}
          after={["a", "B", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n"}
          path="x.txt"
          mode={mode}
          collapseUnchanged={false}
          navRef={navRef}
          onChunkPosition={onChunkPosition}
        />,
      );
      await waitFor(() => expect(navRef.current).not.toBeNull());
      const { EditorView } = await import("@codemirror/view");
      const content = getByTestId("code-diff-review").querySelectorAll(".cm-content");
      const view = EditorView.findFromDOM(content[content.length - 1] as HTMLElement)!;
      // Column 0 of "j": CM reads it as inside the change on "I" above it.
      act(() => view.dispatch({ selection: { anchor: view.state.doc.line(10).from } }));
      act(() => navRef.current!.prev());
      expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 1, count: 2 });
    });
  }

  it('"previous" from a pure deletion steps to the change before it', async () => {
    const onChunkPosition = vi.fn();
    const navRef = { current: null as Nav | null };
    // "B" replaced, "i" deleted: the deletion is an empty range on the b side.
    const deleted = ["a", "B", "c", "d", "e", "f", "g", "h", "j"].join("\n") + "\n";
    const { getByTestId } = render(
      <CodeDiffReview
        before={before}
        after={deleted}
        path="x.txt"
        collapseUnchanged={false}
        navRef={navRef}
        onChunkPosition={onChunkPosition}
      />,
    );
    await waitFor(() => expect(navRef.current).not.toBeNull());
    const { EditorView } = await import("@codemirror/view");
    const content = getByTestId("code-diff-review").querySelectorAll(".cm-content");
    const view = EditorView.findFromDOM(content[content.length - 1] as HTMLElement)!;
    // Column 0 of "j", where the deleted "i" was.
    act(() => view.dispatch({ selection: { anchor: view.state.doc.line(9).from } }));
    act(() => navRef.current!.prev());
    expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 0, count: 2 });
  });

  it("a click inside the diff re-reports where the reader is", async () => {
    const onChunkPosition = vi.fn();
    const navRef = { current: null as Nav | null };
    const { getByTestId } = render(
      <CodeDiffReview
        before={before}
        after={two}
        path="x.txt"
        collapseUnchanged={false}
        navRef={navRef}
        onChunkPosition={onChunkPosition}
      />,
    );
    await waitFor(() => expect(navRef.current).not.toBeNull());
    await waitFor(() => expect(onChunkPosition).toHaveBeenCalled());
    // A same-place click still reports: the host may have cleared its
    // counter on a switch that kept this view.
    const calls = onChunkPosition.mock.calls.length;
    fireEvent.mouseUp(getByTestId("code-diff-review"));
    expect(onChunkPosition.mock.calls.length).toBe(calls + 1);
    expect(onChunkPosition).toHaveBeenLastCalledWith({ index: null, count: 2 });
    // The caret lands in the second change (as a click or arrow key would).
    const { EditorView } = await import("@codemirror/view");
    const content = getByTestId("code-diff-review").querySelectorAll(".cm-content");
    const view = EditorView.findFromDOM(content[content.length - 1] as HTMLElement)!;
    act(() => view.dispatch({ selection: { anchor: view.state.doc.line(9).from } }));
    fireEvent.keyUp(getByTestId("code-diff-review"), { key: "ArrowDown" });
    expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 1, count: 2 });
  });

  it("a click that does not move the caret leaves the first next on the top change", async () => {
    const top = ["A", "b", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";
    const onChunkPosition = vi.fn();
    const navRef = { current: null as Nav | null };
    const { getByTestId } = render(
      <CodeDiffReview
        before={before}
        after={top}
        path="x.txt"
        collapseUnchanged={false}
        navRef={navRef}
        onChunkPosition={onChunkPosition}
      />,
    );
    await waitFor(() => expect(navRef.current).not.toBeNull());
    // A click that leaves the caret where it was (a scrollbar) is not a move.
    fireEvent.mouseUp(getByTestId("code-diff-review"));
    act(() => navRef.current!.next());
    expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 0, count: 2 });
  });

  it("after a click inside the top change, the first next moves on to the second", async () => {
    const top = ["A", "b", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";
    const onChunkPosition = vi.fn();
    const navRef = { current: null as Nav | null };
    const { getByTestId } = render(
      <CodeDiffReview
        before={before}
        after={top}
        path="x.txt"
        collapseUnchanged={false}
        navRef={navRef}
        onChunkPosition={onChunkPosition}
      />,
    );
    await waitFor(() => expect(navRef.current).not.toBeNull());
    const { EditorView } = await import("@codemirror/view");
    const content = getByTestId("code-diff-review").querySelectorAll(".cm-content");
    const view = EditorView.findFromDOM(content[content.length - 1] as HTMLElement)!;
    act(() => view.dispatch({ selection: { anchor: 1 } }));
    fireEvent.mouseUp(getByTestId("code-diff-review"));
    act(() => navRef.current!.next());
    expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 1, count: 2 });
  });

  it("a rebuilt view (split to inline) lands the first next on a top change again", async () => {
    const top = ["A", "b", "c", "d", "e", "f", "g", "h", "I", "j"].join("\n") + "\n";
    const onChunkPosition = vi.fn();
    const navRef = { current: null as Nav | null };
    const props = {
      before,
      after: top,
      path: "x.txt",
      collapseUnchanged: false,
      navRef,
      onChunkPosition,
    };
    const { rerender } = render(<CodeDiffReview {...props} mode="split" />);
    await waitFor(() => expect(navRef.current).not.toBeNull());
    act(() => navRef.current!.next());
    act(() => navRef.current!.next());
    const old = navRef.current;
    rerender(<CodeDiffReview {...props} mode="inline" />);
    await waitFor(() => expect(navRef.current).not.toBe(old));
    await waitFor(() => expect(navRef.current).not.toBeNull());
    act(() => navRef.current!.next());
    expect(onChunkPosition).toHaveBeenLastCalledWith({ index: 0, count: 2 });
  });
});

describe("CodeDiffReview — an inline onChunkPosition (review round 3)", () => {
  it("a host passing a fresh state-setting callback each render does not loop", async () => {
    const before = "a\nb\nc\n";
    const after = "a\nB\nc\n";
    const counter = { renders: 0 };
    function Host() {
      const [pos, setPos] = React.useState<{ index: number | null; count: number } | null>(null);
      React.useEffect(() => {
        counter.renders += 1;
      });
      return (
        <>
          <span data-testid="count">{pos?.count ?? "-"}</span>
          <CodeDiffReview
            before={before}
            after={after}
            path="x.txt"
            collapseUnchanged={false}
            onChunkPosition={(p) => setPos({ ...p })}
          />
        </>
      );
    }
    render(<Host />);
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("1"), {
      timeout: 5000,
    });
    const settled = counter.renders;
    await new Promise((r) => setTimeout(r, 100));
    expect(counter.renders).toBe(settled);
  });
});

it.each(["split", "inline"] as const)(
  "reveals and selects an inclusive after-side range in %s review",
  async (mode) => {
    const { container } = render(
      <CodeDiffReview
        before={"before\nold\nend"}
        after={"before\nnew\nend"}
        path="a.ts"
        mode={mode}
        collapseUnchanged={false}
        afterRange={{ start: 2, end: 3 }}
      />,
    );
    const { EditorView } = await import("@codemirror/view");
    await waitFor(() => expect(container.querySelector(".cm-content")).toBeTruthy());
    await waitFor(() => {
      const editors = container.querySelectorAll(".cm-content");
      expect(editors.length).toBeGreaterThan(0);
      const view = EditorView.findFromDOM(editors[editors.length - 1] as HTMLElement)!;
      expect(view.state.selection.main.from).toBe(view.state.doc.line(2).from);
      expect(view.state.selection.main.to).toBe(view.state.doc.line(3).to);
    });
  },
);
