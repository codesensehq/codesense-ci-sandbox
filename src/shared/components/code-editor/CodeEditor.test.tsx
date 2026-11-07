// SPDX-License-Identifier: MIT
import { act, render, waitFor, fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createRef } from "react";
import { holdTextMenuFocus } from "@/shared/lib/text-menu-focus";

import { CodeEditor, type CodeEditorHandle } from "./CodeEditor";
import * as cmLoader from "./cm-loader";

// next-themes lives in a single module across all tests; default mock
// returns dark. Individual tests rebind via mockReturnValue when they want
// to flip the theme.
const mockResolvedTheme = vi.fn(() => "dark");
vi.mock("next-themes", () => ({
  useTheme: () => ({ resolvedTheme: mockResolvedTheme() }),
}));

// viewer-store is a real Zustand store but our component only reads two
// fields. Provide a minimal mock so tests don't depend on store init order.
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: (selector: (state: unknown) => unknown) =>
    selector({ settings: { codeEditorFontSize: "normal", codeEditorFontSizePx: 0 } }),
}));

beforeEach(() => {
  cmLoader._resetCmLoaderForTests();
  mockResolvedTheme.mockReturnValue("dark");
});

afterEach(() => {
  vi.restoreAllMocks();
});

/** Waits for the editor's `.cm-editor` root to render (load + mount complete). */
async function waitForReady(container: HTMLElement): Promise<HTMLElement> {
  return await waitFor(
    () => {
      const root = container.querySelector(".cm-editor") as HTMLElement | null;
      if (!root) throw new Error("CM6 not yet mounted");
      return root;
    },
    { timeout: 3000 },
  );
}

describe("CodeEditor — render + lifecycle", () => {
  it("renders the skeleton while CM6 is loading", () => {
    const { container } = render(<CodeEditor value="x" onChange={() => {}} />);
    const skeleton = container.querySelector('[aria-busy="true"]');
    expect(skeleton).toBeTruthy();
    expect(skeleton?.textContent ?? "").toMatch(/Loading editor/i);
  });

  it("transitions from loading to ready (CM6 mounts into container)", async () => {
    const { container } = render(<CodeEditor value="hello" onChange={() => {}} />);
    await waitForReady(container);
    // Skeleton should be gone once CM6 mounts (container becomes visible).
    expect(container.querySelector('[aria-busy="true"]')).toBeFalsy();
  });

  it("honors the height prop on the outer container", () => {
    const { container } = render(<CodeEditor value="" onChange={() => {}} height="500px" />);
    const outer = container.querySelector('[data-codesense-code-editor=""]') as HTMLElement;
    expect(outer?.style.height).toBe("500px");
  });

  it("SW-VE-8: marks the root `data-cm-search` ONLY when find is enabled (⌘F ownership)", () => {
    const off = render(<CodeEditor value="" onChange={() => {}} />);
    expect(
      off.container
        .querySelector('[data-codesense-code-editor=""]')
        ?.hasAttribute("data-cm-search"),
    ).toBe(false);
    const on = render(<CodeEditor value="" onChange={() => {}} search />);
    expect(
      on.container.querySelector('[data-codesense-code-editor=""]')?.hasAttribute("data-cm-search"),
    ).toBe(true);
  });

  it("renders inline fallback + Retry on load failure (Freya G2 / D-19)", async () => {
    const err = new Error("simulated chunk-load failure");
    const onLoadError = vi.fn();
    const realGet = cmLoader.getCodeMirror;
    let callCount = 0;
    const spy = vi.spyOn(cmLoader, "getCodeMirror").mockImplementation(() => {
      callCount += 1;
      if (callCount === 1) return Promise.reject(err);
      return realGet();
    });

    const { container } = render(
      <CodeEditor value="" onChange={() => {}} onLoadError={onLoadError} />,
    );

    // First load rejects → inline fallback rendered.
    await waitFor(() => {
      expect(container.querySelector('[role="alert"]')).toBeTruthy();
    });
    expect(container.querySelector('[role="alert"]')?.textContent ?? "").toMatch(/failed to load/i);
    expect(onLoadError).toHaveBeenCalledWith(err);

    // Retry → second load resolves → editor mounts.
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /retry/i }));
    });
    await waitForReady(container);
    expect(spy).toHaveBeenCalledTimes(2);
  });
});

describe("CodeEditor — editing", () => {
  it("fires onChange when the user types", async () => {
    const onChange = vi.fn();
    const { container } = render(<CodeEditor value="hi" onChange={onChange} />);
    await waitForReady(container);
    const contentDOM = container.querySelector(".cm-content") as HTMLElement;
    contentDOM.focus();
    await userEvent.type(contentDOM, "!");
    await waitFor(() => {
      expect(onChange).toHaveBeenCalled();
    });
    // happy-dom's contenteditable caret positioning isn't 1:1 with real
    // browsers, so we don't pin the exact final string — just that onChange
    // fired with a value containing both the original content and the new
    // character (i.e., insertion happened).
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1][0] as string;
    expect(lastCall).toContain("hi");
    expect(lastCall).toContain("!");
    expect(lastCall.length).toBeGreaterThan("hi".length);
  });

  it("blocks keystrokes when readOnly", async () => {
    const onChange = vi.fn();
    const { container } = render(<CodeEditor value="ro" onChange={onChange} readOnly />);
    await waitForReady(container);
    const contentDOM = container.querySelector(".cm-content") as HTMLElement;
    contentDOM.focus();
    await userEvent.type(contentDOM, "X");
    // Content should not have changed.
    expect(contentDOM.textContent ?? "").toBe("ro");
    expect(onChange).not.toHaveBeenCalled();
  });

  it("syncs external `value` prop updates without echoing onChange (D-15, M-10)", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const onChange = vi.fn();
    const { container, rerender } = render(
      <CodeEditor ref={handleRef} value="foo" onChange={onChange} />,
    );
    await waitForReady(container);
    onChange.mockClear();
    rerender(<CodeEditor ref={handleRef} value="bar" onChange={onChange} />);
    await waitFor(() => {
      expect(handleRef.current?.getValue()).toBe("bar");
    });
    // M-10: programmatic value sync must not echo through onChange.
    expect(onChange).not.toHaveBeenCalled();
  });
});

describe("CodeEditor — save semantics", () => {
  it("calls onSave on Cmd+S and marks the event defaultPrevented (M-6)", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const { container } = render(
      <CodeEditor value="save-me" onChange={() => {}} onSave={onSave} />,
    );
    await waitForReady(container);
    const contentDOM = container.querySelector(".cm-content") as HTMLElement;
    contentDOM.focus();

    // M-6 / iter-2 A-2: the M-6 contract is "global useShortcutHandler is
    // not invoked when CM6 has focus". CM6's keymap framework only sets
    // `event.defaultPrevented` — it does NOT call `stopPropagation`. The
    // real contract codesense relies on is that `useShortcutHandler` gates
    // on `event.defaultPrevented` (a host-integration concern outside
    // SS-14's surface). Here we assert the editor's half of the contract:
    // the keystroke triggers onSave and the event ends up
    // defaultPrevented. Host-side gating is verified in App-level
    // integration tests.
    const event = new KeyboardEvent("keydown", {
      key: "s",
      code: "KeyS",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    contentDOM.dispatchEvent(event);
    await waitFor(() => {
      expect(onSave).toHaveBeenCalledWith("save-me");
    });
    expect(event.defaultPrevented).toBe(true);
  });

  it("dedups concurrent Cmd+S while a prior save is in flight (D-23)", async () => {
    const onSave = vi.fn(() => new Promise<void>(() => {})); // never resolves
    const { container } = render(<CodeEditor value="dedup" onChange={() => {}} onSave={onSave} />);
    await waitForReady(container);
    const contentDOM = container.querySelector(".cm-content") as HTMLElement;
    contentDOM.focus();
    fireEvent.keyDown(contentDOM, { key: "s", code: "KeyS", metaKey: true });
    fireEvent.keyDown(contentDOM, { key: "s", code: "KeyS", metaKey: true });
    fireEvent.keyDown(contentDOM, { key: "s", code: "KeyS", metaKey: true });
    // Iter-2 E2-H2 patch defers the onSave call into a microtask via
    // `Promise.resolve().then(...)`. Flush microtasks before asserting.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it("Cmd+S is a no-op when readOnly", async () => {
    const onSave = vi.fn();
    const { container } = render(
      <CodeEditor value="ro" onChange={() => {}} onSave={onSave} readOnly />,
    );
    await waitForReady(container);
    const contentDOM = container.querySelector(".cm-content") as HTMLElement;
    contentDOM.focus();
    fireEvent.keyDown(contentDOM, { key: "s", code: "KeyS", metaKey: true });
    expect(onSave).not.toHaveBeenCalled();
  });

  it("waits for the text menu to close before saving the latest editor draft", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const { container, rerender } = render(
      <CodeEditor value="before" onChange={() => {}} onSave={onSave} dirty />,
    );
    await waitForReady(container);
    const field = container.querySelector<HTMLElement>(".cm-content")!;
    const finish = holdTextMenuFocus(field);
    fireEvent.blur(field);
    expect(onSave).not.toHaveBeenCalled();
    rerender(<CodeEditor value="after Paste" onChange={() => {}} onSave={onSave} dirty />);
    await waitFor(() => expect(field.textContent).toBe("after Paste"));
    await act(async () => {
      finish(false);
      await new Promise(requestAnimationFrame);
    });
    await waitFor(() => expect(onSave).toHaveBeenCalledExactlyOnceWith("after Paste"));
  });

  it("blur triggers onSave IFF dirty=true", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const { container, rerender } = render(
      <CodeEditor value="clean" onChange={() => {}} onSave={onSave} dirty={false} />,
    );
    await waitForReady(container);
    const contentDOM = container.querySelector(".cm-content") as HTMLElement;
    contentDOM.focus();
    fireEvent.blur(contentDOM);
    expect(onSave).not.toHaveBeenCalled();

    rerender(<CodeEditor value="dirty" onChange={() => {}} onSave={onSave} dirty={true} />);
    await waitFor(() => expect(contentDOM.textContent ?? "").toBe("dirty"));
    contentDOM.focus();
    fireEvent.blur(contentDOM);
    await waitFor(() => {
      expect(onSave).toHaveBeenCalledWith("dirty");
    });
  });
});

describe("CodeEditor — ARIA + theme", () => {
  it("reflects aria-readonly on the editor root (D-21)", async () => {
    const { container, rerender } = render(<CodeEditor value="" onChange={() => {}} />);
    const editorRoot = await waitForReady(container);
    expect(editorRoot.getAttribute("aria-readonly")).toBe("false");

    rerender(<CodeEditor value="" onChange={() => {}} readOnly />);
    await waitFor(() => {
      expect(editorRoot.getAttribute("aria-readonly")).toBe("true");
    });
  });

  it("aria-live region is empty on first mount, populates on readOnly transition (L-11)", async () => {
    const { container, rerender } = render(<CodeEditor value="" onChange={() => {}} readOnly />);
    const live = container.querySelector('[aria-live="polite"]');
    expect(live).toBeTruthy();
    // First mount: no announcement (silent so screen readers don't fire on every editor open).
    expect(live?.textContent ?? "").toBe("");

    // Transition readOnly → false; assert announcement appears.
    rerender(<CodeEditor value="" onChange={() => {}} readOnly={false} />);
    await waitFor(() => {
      expect(container.querySelector('[aria-live="polite"]')?.textContent ?? "").toMatch(
        /editable/i,
      );
    });
  });

  it("sets color-scheme matching the active theme (D-20, M-8)", async () => {
    // M-8 note: spec wants `getComputedStyle(editorRoot).backgroundColor`
    // assertions, but happy-dom doesn't resolve `var(--background)` to
    // a computed color. `style.colorScheme` is the observable proxy and is
    // set atomically with the theme-extension reconfigure (D-20). Visual
    // verification of actual colors is local-only.
    const { container } = render(<CodeEditor value="" onChange={() => {}} theme="dark" />);
    const editorRoot = await waitForReady(container);
    expect(editorRoot.style.colorScheme).toBe("dark");
  });

  it("updates color-scheme when theme prop changes", async () => {
    const { container, rerender } = render(
      <CodeEditor value="" onChange={() => {}} theme="dark" />,
    );
    const editorRoot = await waitForReady(container);
    expect(editorRoot.style.colorScheme).toBe("dark");
    rerender(<CodeEditor value="" onChange={() => {}} theme="light" />);
    await waitFor(() => {
      expect(editorRoot.style.colorScheme).toBe("light");
    });
  });

  it("follows next-themes resolvedTheme when theme='auto'", async () => {
    mockResolvedTheme.mockReturnValue("light");
    const { container } = render(<CodeEditor value="" onChange={() => {}} theme="auto" />);
    const editorRoot = await waitForReady(container);
    expect(editorRoot.style.colorScheme).toBe("light");
  });

  it("explicit theme prop overrides next-themes resolvedTheme (D-16)", async () => {
    mockResolvedTheme.mockReturnValue("light");
    const { container } = render(<CodeEditor value="" onChange={() => {}} theme="dark" />);
    const editorRoot = await waitForReady(container);
    expect(editorRoot.style.colorScheme).toBe("dark");
  });
});

describe("CodeEditor — gutters (D-22)", () => {
  it("renders gutters by default", async () => {
    const { container } = render(<CodeEditor value="line1\nline2" onChange={() => {}} />);
    await waitForReady(container);
    expect(container.querySelector(".cm-gutters")).toBeTruthy();
  });

  it("omits gutters when gutters={false}", async () => {
    const { container } = render(
      <CodeEditor value="line1\nline2" onChange={() => {}} gutters={false} />,
    );
    await waitForReady(container);
    expect(container.querySelector(".cm-gutters")).toBeFalsy();
  });
});

describe("CodeEditor — imperative handle", () => {
  it("checkConflict returns true on mismatch, false on match (AC-4 / D-6)", async () => {
    const handleRef = createRef<CodeEditorHandle>();
    const { container } = render(<CodeEditor ref={handleRef} value="local" onChange={() => {}} />);
    await waitForReady(container);
    expect(handleRef.current?.checkConflict("disk")).toBe(true);
    expect(handleRef.current?.checkConflict("local")).toBe(false);
  });

  it("getState/setState preserves content + cursor + history (D-18, M-9)", async () => {
    const handleA = createRef<CodeEditorHandle | null>();
    const handleB = createRef<CodeEditorHandle | null>();
    const { container: containerA } = render(
      <CodeEditor ref={handleA} value={"alpha\nbeta\ngamma"} onChange={() => {}} />,
    );
    const { container: containerB } = render(
      <CodeEditor ref={handleB} value="from-B" onChange={() => {}} />,
    );
    await waitForReady(containerA);
    await waitForReady(containerB);

    // Type into A so undo history has something to revert to.
    handleA.current!.setSelection({
      from: { line: 0, ch: 5 },
      to: { line: 0, ch: 5 },
    });
    const aRoot = containerA.querySelector(".cm-content") as HTMLElement;
    aRoot.focus();
    await userEvent.type(aRoot, "X");
    // Under a loaded full-suite run the typed edit can land after the default 1s wait.
    await waitFor(
      () => {
        expect(handleA.current!.getValue()).not.toBe("alpha\nbeta\ngamma");
      },
      { timeout: 5_000 },
    );

    // Move cursor to a known position on line 1.
    handleA.current!.setSelection({
      from: { line: 1, ch: 2 },
      to: { line: 1, ch: 2 },
    });

    const stateA = handleA.current!.getState();
    const valueA = handleA.current!.getValue();
    const cursorA = handleA.current!.getCursor();

    // Transplant A's state into B.
    handleB.current!.setState(stateA!);
    expect(handleB.current!.getValue()).toBe(valueA);
    expect(handleB.current!.getCursor()).toEqual(cursorA);

    // A-3 scroll preservation: CM6's EditorState does NOT carry the DOM
    // scroll position — scrollTop lives on the scrollDOM element, which
    // is per-EditorView, not per-state. Hosts stash scroll separately
    // alongside getState/setState. This test exercises the imperative
    // get/scrollTo pair on a single editor (the scope where the contract
    // is meaningful). happy-dom may clamp scrollTop to 0 due to no layout
    // — we accept that and just assert the round-trip is consistent.
    handleA.current!.scrollTo(42);
    const scrollA = handleA.current!.getScrollPos();
    handleA.current!.scrollTo(scrollA);
    expect(handleA.current!.getScrollPos()).toBe(scrollA);

    // Undo history rides along: dispatching Cmd+Z on B's editor should
    // revert the typed character because A's history was preserved.
    const bRoot = containerB.querySelector(".cm-content") as HTMLElement;
    bRoot.focus();
    fireEvent.keyDown(bRoot, { key: "z", code: "KeyZ", metaKey: true });
    await waitFor(() => {
      expect(handleB.current!.getValue()).toBe("alpha\nbeta\ngamma");
    });
  });

  it("setValue + clearHistory replaces content and resets history", async () => {
    const handleRef = createRef<CodeEditorHandle>();
    const { container } = render(
      <CodeEditor ref={handleRef} value="initial" onChange={() => {}} />,
    );
    await waitForReady(container);
    handleRef.current!.setValue("replaced");
    handleRef.current!.clearHistory();
    expect(handleRef.current!.getValue()).toBe("replaced");
  });

  it("getCursor and setSelection are coherent", async () => {
    const handleRef = createRef<CodeEditorHandle>();
    const { container } = render(
      <CodeEditor ref={handleRef} value={"abc\ndef\nghi"} onChange={() => {}} />,
    );
    await waitForReady(container);
    handleRef.current!.setSelection({
      from: { line: 1, ch: 1 },
      to: { line: 1, ch: 2 },
    });
    const cursor = handleRef.current!.getCursor();
    expect(cursor.line).toBe(1);
    expect(cursor.ch).toBe(2);
  });

  it("SW-VE-9: isSearchOpen/closeSearch reflect + control the find panel (AC-1)", async () => {
    const handleRef = createRef<CodeEditorHandle>();
    const { container } = render(
      <CodeEditor ref={handleRef} value={"line one\nline two\n"} onChange={() => {}} search />,
    );
    const root = await waitForReady(container);
    const content = root.querySelector(".cm-content") as HTMLElement;
    content.focus();

    // Closed initially: query is false, close is a no-op returning false.
    expect(handleRef.current!.isSearchOpen()).toBe(false);
    expect(handleRef.current!.closeSearch()).toBe(false);

    // ⌘F opens the panel via the real searchKeymap (precedent: the ⌘S/⌘Z
    // keydown tests against real CM6 above).
    fireEvent.keyDown(content, { key: "f", code: "KeyF", metaKey: true });
    expect(handleRef.current!.isSearchOpen()).toBe(true);

    // closeSearch closes it and reports that it did; a second call is a no-op.
    expect(handleRef.current!.closeSearch()).toBe(true);
    expect(handleRef.current!.isSearchOpen()).toBe(false);
    expect(handleRef.current!.closeSearch()).toBe(false);
  });

  it("SW-VE-9: isSearchOpen/closeSearch are null-safe after unmount (M-2)", async () => {
    const handleRef = createRef<CodeEditorHandle>();
    const { container, unmount } = render(
      <CodeEditor ref={handleRef} value="x" onChange={() => {}} search />,
    );
    await waitForReady(container);
    const captured = handleRef.current!;
    unmount();
    // Methods on the captured handle degrade to false once the view is gone.
    expect(captured.isSearchOpen()).toBe(false);
    expect(captured.closeSearch()).toBe(false);
  });
});

describe("CodeEditor — multi-instance independence (AC-5)", () => {
  it("three instances maintain independent state", async () => {
    const onChangeA = vi.fn();
    const onChangeB = vi.fn();
    const onChangeC = vi.fn();
    const { container } = render(
      <div>
        <CodeEditor value="a" onChange={onChangeA} />
        <CodeEditor value="b" onChange={onChangeB} />
        <CodeEditor value="c" onChange={onChangeC} />
      </div>,
    );
    // Wait for all three to mount.
    await waitFor(() => {
      expect(container.querySelectorAll(".cm-editor").length).toBe(3);
    });
    const [domA, domB, domC] = Array.from(
      container.querySelectorAll(".cm-content"),
    ) as HTMLElement[];
    expect(domA.textContent).toBe("a");
    expect(domB.textContent).toBe("b");
    expect(domC.textContent).toBe("c");

    domA.focus();
    await userEvent.type(domA, "X");

    await waitFor(() => {
      expect(onChangeA).toHaveBeenCalled();
    });
    expect(onChangeB).not.toHaveBeenCalled();
    expect(onChangeC).not.toHaveBeenCalled();
    expect(domB.textContent).toBe("b");
    expect(domC.textContent).toBe("c");
  });
});

describe("CodeEditor — lazy load (AC-6, L-5)", () => {
  it("singleton invariant: getCodeMirror() returns the same promise across calls", () => {
    // L-5 patch: assert the singleton invariant directly. The previous
    // test used `vi.spyOn(cmLoader, "getCodeMirror")` which couldn't
    // intercept the bound named import inside `use-code-editor.ts`. The
    // singleton check via reference equality is the real contract.
    cmLoader._resetCmLoaderForTests();
    const p1 = cmLoader.getCodeMirror();
    const p2 = cmLoader.getCodeMirror();
    expect(p1).toBe(p2);
  });

  it("two mounts share the same loaded CmModule (cached)", async () => {
    const { container: c1, unmount: u1 } = render(<CodeEditor value="" onChange={() => {}} />);
    await waitForReady(c1);
    u1();
    const { container: c2 } = render(<CodeEditor value="" onChange={() => {}} />);
    await waitForReady(c2);
    // Both mounts succeeded; the singleton served both without crashing.
    expect(c2.querySelector(".cm-editor")).toBeTruthy();
  });
});

describe("CodeEditor — language auto-detect (AC-12, D-17, M-7)", () => {
  /**
   * Active-language assertion: read CM6's `language` facet from the
   * EditorState. The facet returns the active `Language` object (markdown,
   * javascript, etc.) when a language extension is applied, or null for
   * plain text.
   *
   * We dynamically import the facet from `@codemirror/language` instead
   * of binding at module load — keeps the test file resilient to any
   * future restructure of cm-loader's facade.
   */
  async function assertLanguagePresence(
    handle: CodeEditorHandle,
    expectPresent: boolean,
  ): Promise<void> {
    const { language } = await import("@codemirror/language");
    const state = handle.getState();
    expect(state).not.toBeNull();
    const lang = state!.facet(language);
    if (expectPresent) {
      expect(lang).not.toBeNull();
    } else {
      expect(lang).toBeNull();
    }
  }

  it("path='x.md' → markdown active", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={handleRef} value={"# h1"} onChange={() => {}} path="x.md" />,
    );
    await waitForReady(container);
    await assertLanguagePresence(handleRef.current!, true);
  });

  it("path='x.ts' → typescript active", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={handleRef} value="const x = 1;" onChange={() => {}} path="x.ts" />,
    );
    await waitForReady(container);
    await assertLanguagePresence(handleRef.current!, true);
  });

  it("path='x.sh' → shell active (via legacy-modes)", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={handleRef} value="echo hi" onChange={() => {}} path="x.sh" />,
    );
    await waitForReady(container);
    await assertLanguagePresence(handleRef.current!, true);
  });

  it("path with unknown extension → plain (no language extension)", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={handleRef} value="text" onChange={() => {}} path="LICENSE" />,
    );
    await waitForReady(container);
    await assertLanguagePresence(handleRef.current!, false);
  });

  it("path change reconfigures language without crashing", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const { container, rerender } = render(
      <CodeEditor ref={handleRef} value={"# h1"} onChange={() => {}} path="README.md" />,
    );
    await waitForReady(container);
    expect(handleRef.current!.getValue()).toBe("# h1");
    rerender(<CodeEditor ref={handleRef} value={"# h1"} onChange={() => {}} path="x.ts" />);
    await waitFor(() => {
      expect(handleRef.current!.getValue()).toBe("# h1");
    });
  });

  it("explicit language='plain' overrides path-based detection", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={handleRef} value="" onChange={() => {}} path="README.md" language="plain" />,
    );
    await waitForReady(container);
    expect(container.querySelector(".cm-editor")).toBeTruthy();
    await assertLanguagePresence(handleRef.current!, false);
  });

  it("unknown language string narrows to plain (L-4)", async () => {
    const handleRef = createRef<CodeEditorHandle | null>();
    const { container } = render(
      <CodeEditor ref={handleRef} value="" onChange={() => {}} path="x.md" language="brainfuck" />,
    );
    await waitForReady(container);
    // Unknown language id treated as plain; no markdown active.
    await assertLanguagePresence(handleRef.current!, false);
  });
});

describe("CodeEditor — destroy on unmount (AC-15, H-6)", () => {
  it("destroys 100 real EditorViews exactly once across serial and batched unmounts", async () => {
    // H-6 patch: spec / D-14 mandate 100 cycles and strict equality. The
    // previous test ran 5 cycles with `>=`, which would pass even on a
    // pathological double-destroy (correctness regression invisible).
    const cycles = 100;
    const serialCycles = 10;
    const batchSize = 10;
    const destroyCounter = { count: 0 };
    const destroyedViews = new Set<object>();
    const cm = await cmLoader.getCodeMirror();
    const originalDestroy = cm.EditorView.prototype.destroy;
    cm.EditorView.prototype.destroy = function patched(this: { destroy: typeof originalDestroy }) {
      if (destroyedViews.has(this)) throw new Error("EditorView.destroy() called more than once");
      destroyedViews.add(this);
      destroyCounter.count += 1;
      return originalDestroy.call(this);
    };

    try {
      // Preserve repeated React-root churn without making all 100 views pay
      // its serial scheduler cost under full-suite contention.
      for (let index = 0; index < serialCycles; index++) {
        const { container, unmount } = render(<CodeEditor value="" onChange={() => {}} />);
        await waitForReady(container);
        unmount();
        expect(destroyCounter.count).toBe(index + 1);
      }

      // Bounded batches retain the 100-real-view stress invariant. Every
      // wrapper must own exactly one ready CM6 root before teardown, and the
      // cumulative assertion prevents over/under-destroys from cancelling out.
      for (let offset = serialCycles; offset < cycles; offset += batchSize) {
        const currentBatchSize = Math.min(batchSize, cycles - offset);
        const { container, unmount } = render(
          <>
            {Array.from({ length: currentBatchSize }, (_, index) => (
              <CodeEditor key={offset + index} value="" onChange={() => {}} />
            ))}
          </>,
        );
        await waitFor(
          () => {
            const editors = container.querySelectorAll('[data-codesense-code-editor=""]');
            expect(editors).toHaveLength(currentBatchSize);
            for (const editor of editors) {
              expect(editor.querySelectorAll(".cm-editor")).toHaveLength(1);
            }
          },
          { timeout: 10_000 },
        );
        unmount();
        expect(destroyCounter.count).toBe(offset + currentBatchSize);
      }
      expect(destroyCounter.count).toBe(cycles);
      expect(destroyedViews.size).toBe(cycles);
    } finally {
      cm.EditorView.prototype.destroy = originalDestroy;
    }
    // Keep a generous outer budget for loaded workers: a full local suite run has taken
    // over two minutes here. Batching keeps the normal runtime low without weakening the
    // 100-view check.
  }, 300_000);
});
