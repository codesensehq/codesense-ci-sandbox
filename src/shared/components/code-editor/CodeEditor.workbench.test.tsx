// SPDX-License-Identifier: MIT
import { act, render, waitFor } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CodeEditor, type CodeEditorHandle, type CursorInfo } from "./CodeEditor";
import * as cmLoader from "./cm-loader";

// The worktree editor's opt-in affordances on the shared <CodeEditor>: the
// change gutter, CRLF preservation, tab indentation, cursor readout and the
// imperative reveal/find calls the workbench chrome drives.

vi.mock("next-themes", () => ({ useTheme: () => ({ resolvedTheme: "dark" }) }));
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: (selector: (state: unknown) => unknown) =>
    selector({ settings: { codeEditorFontSize: "normal", codeEditorFontSizePx: 0 } }),
}));

beforeEach(() => {
  cmLoader._resetCmLoaderForTests();
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

async function ready(container: HTMLElement): Promise<HTMLElement> {
  return waitFor(
    () => {
      const root = container.querySelector(".cm-editor") as HTMLElement | null;
      if (!root) throw new Error("not mounted");
      return root;
    },
    { timeout: 3000 },
  );
}

describe("CodeEditor — change gutter", () => {
  it("marks lines changed against the committed baseline", async () => {
    const { container } = render(
      <CodeEditor
        value={"one\nTWO\nthree\nfour"}
        onChange={() => {}}
        changeBaseline={"one\ntwo\nthree"}
      />,
    );
    await ready(container);
    await waitFor(() => {
      expect(container.querySelector(".cm-change-gutter .cm-change-modified")).toBeTruthy();
      expect(container.querySelector(".cm-change-gutter .cm-change-added")).toBeTruthy();
    });
    expect(container.querySelector(".cm-overview-ruler .cm-change-modified")).toBeTruthy();
  });

  it("shows no gutter when no baseline is tracked, and none-marked for a null baseline", async () => {
    const off = render(<CodeEditor value="a" onChange={() => {}} />);
    await ready(off.container);
    expect(off.container.querySelector(".cm-change-gutter")).toBeNull();

    const fresh = render(<CodeEditor value="a" onChange={() => {}} changeBaseline={null} />);
    await ready(fresh.container);
    expect(fresh.container.querySelector(".cm-change-gutter")).toBeTruthy();
    expect(fresh.container.querySelector(".cm-change-mark")).toBeNull();
  });

  it("re-marks when the baseline arrives after mount", async () => {
    const { container, rerender } = render(
      <CodeEditor value={"a\nb"} onChange={() => {}} changeBaseline={null} />,
    );
    await ready(container);
    rerender(<CodeEditor value={"a\nb"} onChange={() => {}} changeBaseline={"a"} />);
    await waitFor(() =>
      expect(container.querySelector(".cm-change-gutter .cm-change-added")).toBeTruthy(),
    );
  });
});

describe("CodeEditor — props that settle while CodeMirror loads", () => {
  it("applies a baseline and CRLF that arrive before the view exists", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container, rerender } = render(
      <CodeEditor ref={ref} value={"a\r\nB\r\n"} onChange={() => {}} />,
    );
    // Same tick as mount: the lazy CodeMirror chunk hasn't resolved yet.
    rerender(
      <CodeEditor
        ref={ref}
        value={"a\r\nB\r\n"}
        onChange={() => {}}
        lineSeparator={"\r\n"}
        changeBaseline={"a\r\nb\r\n"}
      />,
    );
    await ready(container);
    await waitFor(() =>
      expect(container.querySelector(".cm-change-gutter .cm-change-modified")).toBeTruthy(),
    );
    expect(ref.current!.getValue()).toBe("a\r\nB\r\n");
  });
});

describe("CodeEditor — file format fidelity", () => {
  it("keeps CRLF line endings when the host passes the file's separator", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={ref} value={"a\r\nb\r\n"} onChange={() => {}} lineSeparator={"\r\n"} />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    expect(ref.current!.getValue()).toBe("a\r\nb\r\n");
  });

  it("turns LF text inserted into a CRLF file into CRLF breaks (no mixed endings)", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const onChange = vi.fn();
    const { container } = render(
      <CodeEditor ref={ref} value={"a\r\nb\r\n"} onChange={onChange} lineSeparator={"\r\n"} />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    act(() => {
      // A paste of LF text at the end of line 1.
      const state = ref.current!.getState()!;
      const tr = state.update({ changes: { from: 1, insert: "\nx\ny" }, userEvent: "input.paste" });
      ref.current!.setState(tr.state);
    });
    expect(ref.current!.getValue()).toBe("a\r\nx\r\ny\r\nb\r\n");
  });

  it("hands typed text and ⌘S to the host with the file's CRLF endings", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const onChange = vi.fn();
    const onSave = vi.fn();
    const { container } = render(
      <CodeEditor
        ref={ref}
        value={"a\r\nb\r\n"}
        onChange={onChange}
        onSave={onSave}
        lineSeparator={"\r\n"}
      />,
    );
    const root = await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    const { EditorView } = await import("@codemirror/view");
    const view = EditorView.findFromDOM(root)!;
    act(() => {
      // Typing through the view (not setState) reaches the update listener.
      view.dispatch({ changes: { from: 1, insert: "\nz" }, userEvent: "input.type" });
    });
    expect(onChange).toHaveBeenLastCalledWith("a\r\nz\r\nb\r\n");
    act(() => {
      view.contentDOM.dispatchEvent(
        new KeyboardEvent("keydown", { key: "s", metaKey: true, bubbles: true, cancelable: true }),
      );
    });
    await waitFor(() => expect(onSave).toHaveBeenCalledWith("a\r\nz\r\nb\r\n"));
  });

  it("undoing a delete in a CRLF file puts a lone CR back byte-for-byte", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={ref} value={"a\rb\r\nc\r\n"} onChange={() => {}} lineSeparator={"\r\n"} />,
    );
    const root = await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    const { EditorView } = await import("@codemirror/view");
    const { undo } = await import("@codemirror/commands");
    const view = EditorView.findFromDOM(root)!;
    act(() => {
      view.dispatch({ changes: { from: 0, to: 3 }, userEvent: "delete" });
    });
    expect(ref.current!.getValue()).toBe("\r\nc\r\n");
    act(() => {
      undo(view);
    });
    expect(ref.current!.getValue()).toBe("a\rb\r\nc\r\n");
  });

  it("syncs an external value into a CRLF file without a range error", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container, rerender } = render(
      <CodeEditor ref={ref} value={"a\r\nb\r\nc\r\n"} onChange={() => {}} lineSeparator={"\r\n"} />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    rerender(
      <CodeEditor ref={ref} value={"a\r\nB\r\nc\r\n"} onChange={() => {}} lineSeparator={"\r\n"} />,
    );
    await waitFor(() => expect(ref.current!.getValue()).toBe("a\r\nB\r\nc\r\n"));
  });

  it("keeps a lone CR inside a CRLF file byte-for-byte on mount and on reload", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container, rerender } = render(
      <CodeEditor ref={ref} value={"a\rb\r\nc\r\n"} onChange={() => {}} lineSeparator={"\r\n"} />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    expect(ref.current!.getValue()).toBe("a\rb\r\nc\r\n");
    const linesOnMount = ref.current!.getState()!.doc.lines;
    rerender(
      <CodeEditor ref={ref} value={"a\rB\r\nc\r\n"} onChange={() => {}} lineSeparator={"\r\n"} />,
    );
    await waitFor(() => expect(ref.current!.getValue()).toBe("a\rB\r\nc\r\n"));
    expect(ref.current!.getState()!.doc.lines).toBe(linesOnMount);
  });

  it("re-splits a reload that switches the file from CRLF to LF", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container, rerender } = render(
      <CodeEditor ref={ref} value={"a\r\nb\r\n"} onChange={() => {}} lineSeparator={"\r\n"} />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    rerender(<CodeEditor ref={ref} value={"a\nb\nc\n"} onChange={() => {}} lineSeparator={"\n"} />);
    await waitFor(() => expect(ref.current!.getValue()).toBe("a\nb\nc\n"));
    expect(ref.current!.getState()!.doc.lines).toBe(4);
  });

  it("indents with a hard tab when asked", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const onChange = vi.fn();
    const { container } = render(
      <CodeEditor ref={ref} value={"x"} onChange={onChange} ide indentWithTabs tabSize={4} />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    const content = container.querySelector(".cm-content") as HTMLElement;
    act(() => {
      ref.current!.focus();
      content.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    });
    await waitFor(() => expect(onChange).toHaveBeenLastCalledWith("\tx"));
  });
});

describe("CodeEditor — cursor + navigation", () => {
  it("reports the cursor and moves it with revealLine", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const seen: CursorInfo[] = [];
    const { container } = render(
      <CodeEditor
        ref={ref}
        value={"alpha\nbeta\ngamma"}
        onChange={() => {}}
        onCursorChange={(info) => seen.push(info)}
      />,
    );
    await ready(container);
    await waitFor(() => expect(seen.length).toBeGreaterThan(0));
    expect(seen.at(-1)).toMatchObject({ line: 1, column: 1, selections: 1, selectedChars: 0 });
    act(() => ref.current!.revealLine(3, 3));
    await waitFor(() => expect(seen.at(-1)).toMatchObject({ line: 3, column: 3 }));
    // Out-of-range input clamps instead of throwing.
    act(() => ref.current!.revealLine(99, 99));
    await waitFor(() => expect(seen.at(-1)).toMatchObject({ line: 3, column: 6 }));
  });

  it("resolves the enclosing scope for the breadcrumb", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const seen: CursorInfo[] = [];
    const { container } = render(
      <CodeEditor
        ref={ref}
        path="a.ts"
        value={"class Foo {\n  bar() {\n    return 1;\n  }\n}\n"}
        onChange={() => {}}
        onCursorChange={(info) => seen.push(info)}
      />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    act(() => ref.current!.revealLine(3, 5));
    await waitFor(() => expect(seen.at(-1)?.scope).toEqual(["class Foo", "method bar"]));
  });

  it("opens the find panel on request", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={ref} value="needle" onChange={() => {}} search />,
    );
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    act(() => ref.current!.openSearch());
    expect(ref.current!.isSearchOpen()).toBe(true);
    expect(container.querySelector(".cm-search")).toBeTruthy();
    // The panel speaks the app's words, in sentence case.
    expect(container.querySelector(".cm-editor")).toHaveClass("cs-editor-search");
    const labels = Array.from(container.querySelectorAll(".cm-search label"), (l) =>
      l.textContent?.trim(),
    );
    expect(labels).toEqual(["Match case", "Regex", "Whole word"]);
    expect(container.querySelector(".cm-search button[name=replaceAll]")).toHaveTextContent(
      "Replace all",
    );
  });
});

describe("CodeEditor — review round 3", () => {
  it("openSearch is a no-op without the search option (no unconfigured panel)", async () => {
    const ref = createRef<CodeEditorHandle | null>();
    const { container } = render(<CodeEditor ref={ref} value="needle" onChange={() => {}} ide />);
    await ready(container);
    await waitFor(() => expect(ref.current).toBeTruthy());
    act(() => ref.current!.openSearch());
    expect(ref.current!.isSearchOpen()).toBe(false);
    expect(container.querySelector(".cm-search")).toBeNull();
  });

  it("the overview ruler never places a mark below the ruler after lines are deleted", async () => {
    const { container, rerender } = render(
      <CodeEditor value={"a\nb\nc\nd\ne"} onChange={() => {}} changeBaseline={"a"} />,
    );
    await ready(container);
    await waitFor(() =>
      expect(container.querySelector(".cm-overview-ruler .cm-change-added")).toBeTruthy(),
    );
    // The doc shrinks; the changes still name lines 2–5 until they recompute.
    rerender(<CodeEditor value={"a"} onChange={() => {}} changeBaseline={"a"} />);
    const tops = Array.from(container.querySelectorAll<HTMLElement>(".cm-overview-mark"), (m) =>
      parseFloat(m.style.top),
    );
    expect(tops.every((t) => t <= 100)).toBe(true);
  });
});
