// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { createFlashController, findScrollParent, minimalChange } from "./live-diff";
import { getCodeMirror, _resetCmLoaderForTests } from "./cm-loader";

/** Apply a DocChange to a string the same way CodeMirror applies it to the doc. */
function apply(current: string, change: ReturnType<typeof minimalChange>): string {
  if (!change) return current;
  return current.slice(0, change.from) + change.insert + current.slice(change.to);
}

describe("minimalChange", () => {
  it("returns null for identical strings (no dispatch)", () => {
    expect(minimalChange("abc", "abc")).toBeNull();
  });

  it("captures a pure append as an insert at the end", () => {
    const change = minimalChange("line1\n", "line1\nline2\n");
    expect(change).toEqual({ from: 6, to: 6, insert: "line2\n" });
  });

  it("captures a pure prepend as an insert at the start", () => {
    const change = minimalChange("world", "hello world");
    expect(change).toEqual({ from: 0, to: 0, insert: "hello " });
  });

  it("captures a middle edit as a tight replaced span", () => {
    const change = minimalChange("const x = 1;", "const x = 42;");
    expect(change).toEqual({ from: 10, to: 11, insert: "42" });
  });

  it("captures a deletion", () => {
    const change = minimalChange("abcdef", "abef");
    expect(change).toEqual({ from: 2, to: 4, insert: "" });
  });

  it("the inserted span [from, from+insert.length) locates the new text (flash range)", () => {
    const change = minimalChange("a\nb\n", "a\nX\nY\nb\n");
    expect(change).not.toBeNull();
    if (change) {
      const flashStart = change.from;
      const flashEnd = change.from + change.insert.length;
      const next = apply("a\nb\n", change);
      expect(next.slice(flashStart, flashEnd)).toBe("X\nY\n");
    }
  });

  it("round-trips: applying the change reproduces the target for many shapes", () => {
    const cases: Array<[string, string]> = [
      ["", "new"],
      ["old", ""],
      ["", ""],
      ["same", "same"],
      ["aXb", "aYb"],
      ["function f() {}", "function f() {\n  return 1;\n}"],
      ["a".repeat(500), "a".repeat(250) + "Z" + "a".repeat(250)],
    ];
    for (const [a, b] of cases) {
      expect(apply(a, minimalChange(a, b))).toBe(b);
    }
  });
});

describe("findScrollParent", () => {
  it("returns the nearest overflow:auto/scroll ancestor, else null", () => {
    const outer = document.createElement("div");
    const scroller = document.createElement("div");
    scroller.style.overflowY = "auto";
    const middle = document.createElement("div"); // overflow visible (default)
    const leaf = document.createElement("div");
    outer.appendChild(scroller);
    scroller.appendChild(middle);
    middle.appendChild(leaf);
    document.body.appendChild(outer);
    try {
      expect(findScrollParent(leaf)).toBe(scroller);
      // A subtree with no scrollable ancestor resolves to null.
      const orphan = document.createElement("div");
      document.body.appendChild(orphan);
      expect(findScrollParent(orphan)).toBeNull();
      document.body.removeChild(orphan);
    } finally {
      document.body.removeChild(outer);
    }
  });

  it("is null-safe for a null element", () => {
    expect(findScrollParent(null)).toBeNull();
  });
});

describe("createFlashController (AC-3)", () => {
  beforeEach(() => {
    _resetCmLoaderForTests();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("adds a flash line decoration on flash() and clears it after the duration", async () => {
    const cm = await getCodeMirror();
    vi.useFakeTimers();
    const flash = createFlashController(cm);
    const view = new cm.EditorView({
      doc: "line1\nline2\nline3\nline4",
      extensions: [flash.extension],
    });
    document.body.appendChild(view.dom);
    try {
      flash.flash(view, 0, 5, 700); // flash the first line's range
      expect(view.dom.querySelectorAll(".cm-live-flash-line").length).toBeGreaterThan(0);

      vi.advanceTimersByTime(700);
      expect(view.dom.querySelectorAll(".cm-live-flash-line").length).toBe(0);
    } finally {
      view.destroy();
      view.dom.remove(); // idempotent — destroy may already have detached it
    }
  });

  it("dispose() cancels pending clear timers (no dispatch against a torn-down view)", async () => {
    const cm = await getCodeMirror();
    vi.useFakeTimers();
    const flash = createFlashController(cm);
    const view = new cm.EditorView({ doc: "a\nb", extensions: [flash.extension] });
    document.body.appendChild(view.dom);
    flash.flash(view, 0, 1, 700);
    view.destroy();
    view.dom.remove();
    const dispatchSpy = vi.spyOn(view, "dispatch");
    flash.dispose();
    // The pending 700ms clear was cancelled — advancing time dispatches nothing.
    vi.advanceTimersByTime(1000);
    expect(dispatchSpy).not.toHaveBeenCalled();
  });
});
