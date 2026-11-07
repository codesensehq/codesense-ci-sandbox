// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";

// Mock the heavy engine so the canvas's reload behavior (AC-2/AC-3) can be
// exercised headless. The fake Excalidraw registers an imperative API and
// surfaces the `onChange` so a test can simulate a user edit; the version
// helpers are stand-ins whose only contract is "different elements → different
// signature".
// The fake API tracks the committed scene so `getSceneElements/getAppState/
// getFiles` reflect what `updateScene` (merge) + `addFiles` (union) left —
// mirroring how the real engine reports post-apply state.
let appliedElements: { id: string }[] = [];
let appliedAppState: Record<string, unknown> = {};
let appliedFiles: Record<string, unknown> = {};
const updateScene = vi.fn(
  (data: {
    elements?: { id: string }[];
    appState?: Record<string, unknown>;
    captureUpdate?: unknown;
  }) => {
    if (data.elements) appliedElements = data.elements;
    if (data.appState) appliedAppState = { ...appliedAppState, ...data.appState };
  },
);
const addFiles = vi.fn((files: { id: string }[]) => {
  for (const f of files) appliedFiles[f.id] = f;
});
const fakeApi = {
  updateScene,
  addFiles,
  getSceneElements: () => appliedElements,
  getAppState: () => appliedAppState,
  getFiles: () => appliedFiles,
};
let latestOnChange: ((els: unknown, appState: unknown, files: unknown) => void) | null = null;

vi.mock("@excalidraw/excalidraw/index.css", () => ({}));
vi.mock("@excalidraw/excalidraw", () => ({
  Excalidraw: (props: {
    excalidrawAPI?: (api: typeof fakeApi) => void;
    onChange?: (els: unknown, appState: unknown, files: unknown) => void;
  }) => {
    props.excalidrawAPI?.(fakeApi);
    latestOnChange = props.onChange ?? null;
    return <div data-testid="fake-excalidraw" />;
  },
  serializeAsJSON: (els: unknown) => JSON.stringify(els),
  hashElementsVersion: (els: { id: string }[]) => `h:${els.map((e) => e.id).join(",")}`,
  CaptureUpdateAction: { NEVER: "never", IMMEDIATELY: "immediately", EVENTUALLY: "eventually" },
}));

const { writeContentFileMock } = vi.hoisted(() => ({
  writeContentFileMock: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/tauri/commands", () => ({ writeContentFile: writeContentFileMock }));
vi.mock("next-themes", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));
vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

// Imported after the mocks so the canvas binds to them.
import ExcalidrawCanvas from "./excalidraw-canvas";
import type { ExcalidrawScene } from "./excalidraw-viewer";

const scene = (ids: string[], appState: Record<string, unknown> = {}): ExcalidrawScene => ({
  elements: ids.map((id) => ({ id })),
  appState,
  files: {},
});

beforeEach(() => {
  updateScene.mockReset();
  addFiles.mockReset();
  writeContentFileMock.mockReset();
  writeContentFileMock.mockResolvedValue(undefined);
  appliedElements = [];
  appliedAppState = {};
  appliedFiles = {};
  latestOnChange = null;
});

describe("ExcalidrawCanvas hot-reload apply/nudge", () => {
  it("applies an external scene in place when the canvas is clean (AC-2)", () => {
    const sceneA = scene(["a"]);
    // External scene carries a persisted key (viewBackgroundColor) AND transient
    // view state (scrollX) — only the former should reach updateScene.
    const sceneB = scene(["b"], { viewBackgroundColor: "#000", scrollX: 999 });

    const { rerender } = render(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={null}
        reloadNonce={0}
      />,
    );
    expect(updateScene).not.toHaveBeenCalled();

    rerender(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={sceneB}
        reloadNonce={1}
      />,
    );

    expect(updateScene).toHaveBeenCalledTimes(1);
    const arg = updateScene.mock.calls[0][0];
    expect(arg.elements).toEqual(sceneB.elements);
    expect(arg.captureUpdate).toBe("never"); // remote write stays out of undo
    // Viewport preserved: persisted key applied, transient view state dropped.
    expect(arg.appState).toEqual({ viewBackgroundColor: "#000" });
    expect(arg.appState).not.toHaveProperty("scrollX");
    // No nudge banner — it applied silently.
    expect(screen.queryByText(/canvas updated on disk/i)).not.toBeInTheDocument();
  });

  it("nudges instead of clobbering when the user has unsaved edits (AC-3)", () => {
    const sceneA = scene(["a"]);
    const sceneB = scene(["b"]);

    const { rerender } = render(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={null}
        reloadNonce={0}
      />,
    );

    // Simulate a local edit so the live scene diverges from disk (dirty).
    act(() => latestOnChange?.([{ id: "a" }, { id: "c" }], {}, {}));

    rerender(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={sceneB}
        reloadNonce={1}
      />,
    );

    // Must NOT auto-apply over the user's work.
    expect(updateScene).not.toHaveBeenCalled();
    const banner = screen.getByText(/canvas updated on disk/i);
    expect(banner).toBeInTheDocument();

    // Accepting the nudge applies the external scene.
    fireEvent.click(screen.getByRole("button", { name: /reload/i }));
    expect(updateScene).toHaveBeenCalledTimes(1);
    expect(updateScene.mock.calls[0][0].elements).toEqual(sceneB.elements);
    expect(screen.queryByText(/canvas updated on disk/i)).not.toBeInTheDocument();
  });

  it("is a no-op when the external write matches what's already on disk (AC-4 echo)", () => {
    const sceneA = scene(["a"]);

    const { rerender } = render(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={null}
        reloadNonce={0}
      />,
    );

    // An external "change" that parses to the same content we already hold.
    rerender(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={scene(["a"])}
        reloadNonce={1}
      />,
    );

    expect(updateScene).not.toHaveBeenCalled();
    expect(screen.queryByText(/canvas updated on disk/i)).not.toBeInTheDocument();
  });

  it("cancels a pending local save when a reload is accepted, so it can't clobber the applied scene", () => {
    vi.useFakeTimers();
    try {
      const sceneA = scene(["a"]);
      const sceneB = scene(["b"]);
      const { rerender } = render(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneA}
          externalScene={null}
          reloadNonce={0}
        />,
      );

      // Local edit → schedules a debounced save of the user's content.
      act(() => latestOnChange?.([{ id: "a" }, { id: "c" }], {}, {}));
      // External write while dirty → nudge.
      rerender(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneA}
          externalScene={sceneB}
          reloadNonce={1}
        />,
      );
      act(() => {
        fireEvent.click(screen.getByRole("button", { name: /reload/i }));
      });

      // Drain the debounce window: the queued local save must have been
      // cancelled, so the user's superseded edits never reach disk.
      act(() => vi.advanceTimersByTime(2000));
      expect(writeContentFileMock).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not write the merged scene back to disk after an apply (echo no-op across appState merge)", () => {
    vi.useFakeTimers();
    try {
      const sceneA = scene(["a"]);
      const { rerender } = render(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneA}
          externalScene={null}
          reloadNonce={0}
        />,
      );

      // Apply external #1 — sets a persisted appState key on the live canvas.
      rerender(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneA}
          externalScene={scene(["b"], { viewBackgroundColor: "#000" })}
          reloadNonce={1}
        />,
      );
      // Apply external #2 — OMITS viewBackgroundColor; updateScene's merge
      // retains "#000", so the live state differs from the input scene.
      rerender(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneA}
          externalScene={scene(["c"])}
          reloadNonce={2}
        />,
      );

      // Excalidraw fires onChange after updateScene with the committed (merged)
      // state. The baseline was seeded from that same committed state, so this
      // is a pure echo — no write-back re-introducing the retained background.
      act(() => latestOnChange?.(appliedElements, appliedAppState, appliedFiles));
      act(() => vi.advanceTimersByTime(2000));

      expect(writeContentFileMock).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("persists a revert made before a prior edit's save fires (the undo wins, not the abandoned edit)", () => {
    vi.useFakeTimers();
    try {
      const sceneA = scene(["a"]);
      render(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneA}
          externalScene={null}
          reloadNonce={0}
        />,
      );

      // Edit to [a,b] → schedules a debounced save of [a,b].
      act(() => latestOnChange?.([{ id: "a" }, { id: "b" }], {}, {}));
      // Undo back to [a] (the on-disk baseline) before the debounce fires.
      act(() => latestOnChange?.([{ id: "a" }], {}, {}));
      // Drain the debounce: the queued [a,b] write must have been superseded by
      // the revert, so disk ends at [a] — the abandoned edit never wins.
      act(() => vi.advanceTimersByTime(2000));

      expect(writeContentFileMock).toHaveBeenCalled();
      const lastJson = writeContentFileMock.mock.calls.at(-1)?.[1];
      expect(lastJson).toBe(JSON.stringify([{ id: "a" }]));
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears a stale nudge when a later external write matches the disk baseline", () => {
    const sceneA = scene(["a"]);
    const sceneB = scene(["b"]);
    const { rerender } = render(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={null}
        reloadNonce={0}
      />,
    );

    // Dirty + a differing external write → nudge open.
    act(() => latestOnChange?.([{ id: "a" }, { id: "c" }], {}, {}));
    rerender(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={sceneB}
        reloadNonce={1}
      />,
    );
    expect(screen.getByText(/canvas updated on disk/i)).toBeInTheDocument();

    // A newer external write reverts disk to our saved baseline (noop) → the
    // stale banner (pointing at the intermediate scene) must clear.
    rerender(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={scene(["a"])}
        reloadNonce={2}
      />,
    );
    expect(screen.queryByText(/canvas updated on disk/i)).not.toBeInTheDocument();
  });

  // SW-SC-4 AC-8: the mid-draw conflict surfaces as a NON-MODAL banner — its
  // outer layer is pointer-events-none and only the banner box itself is
  // pointer-events-auto, so the canvas stays interactive (drawing continues)
  // underneath while the user decides. A modal overlay would block the UI.
  it("renders the reload nudge non-modally so the canvas stays interactive underneath (AC-8)", () => {
    const sceneA = scene(["a"]);
    const { rerender } = render(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={null}
        reloadNonce={0}
      />,
    );
    // Dirty + differing external write → nudge.
    act(() => latestOnChange?.([{ id: "a" }, { id: "c" }], {}, {}));
    rerender(
      <ExcalidrawCanvas
        filePath="/x.excalidraw"
        scene={sceneA}
        externalScene={scene(["b"])}
        reloadNonce={1}
      />,
    );

    const label = screen.getByText(/canvas updated on disk/i);
    const banner = label.parentElement!; // the interactive banner box
    const overlay = banner.parentElement!; // the full-width positioning layer
    expect(overlay).toHaveClass("pointer-events-none");
    expect(banner).toHaveClass("pointer-events-auto");
    // The canvas itself is still mounted and not covered by a blocking layer.
    expect(screen.getByTestId("fake-excalidraw")).toBeInTheDocument();
  });
});

// SW-SC-4 AC-3 (co-owned delete) + AC-4 (anonymous ink): a delete is just an
// onChange with fewer elements and is persisted like any other edit — there is
// no per-shape ownership lock — and the persisted JSON carries no authorship
// metadata (it's Excalidraw's standard serializer; codesense injects nothing).
describe("ExcalidrawCanvas co-owned delete + anonymous ink", () => {
  it("persists a delete (fewer elements) with no per-shape ownership gate (AC-3)", () => {
    vi.useFakeTimers();
    try {
      const sceneAB = scene(["a", "b"]);
      render(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneAB}
          externalScene={null}
          reloadNonce={0}
        />,
      );

      // Delete "b" — regardless of which party drew it; nothing blocks it.
      act(() => latestOnChange?.([{ id: "a" }], {}, {}));
      act(() => vi.advanceTimersByTime(2000));

      expect(writeContentFileMock).toHaveBeenCalledTimes(1);
      const [path, json] = writeContentFileMock.mock.calls[0];
      expect(path).toBe("/x.excalidraw");
      // The fake serializer is JSON.stringify(elements): the delete persisted
      // exactly the remaining element, proving the write reflects the deletion.
      expect(json).toBe(JSON.stringify([{ id: "a" }]));
    } finally {
      vi.useRealTimers();
    }
  });

  it("writes the serializer output verbatim — no codesense-injected authorship (AC-4)", () => {
    vi.useFakeTimers();
    try {
      const sceneA = scene(["a"]);
      render(
        <ExcalidrawCanvas
          filePath="/x.excalidraw"
          scene={sceneA}
          externalScene={null}
          reloadNonce={0}
        />,
      );

      // Elements deliberately carry arbitrary fields. The guard is that the
      // component persists exactly what `serializeAsJSON` produced — it neither
      // strips fields nor wraps/annotates them with authorship. (Anonymity of
      // the on-disk shape is then Excalidraw's serializer contract, which has
      // no author field — codesense adds nothing around it.) The fake
      // serializer is `JSON.stringify(elements)`, so verbatim pass-through means
      // the written JSON parses back to exactly the input elements.
      const elements = [
        { id: "a", type: "rectangle" },
        { id: "b", type: "text" },
      ];
      act(() => latestOnChange?.(elements, {}, {}));
      act(() => vi.advanceTimersByTime(2000));

      const json = writeContentFileMock.mock.calls.at(-1)?.[1] as string;
      // Verbatim pass-through: no fields added (authorship/provenance) or removed.
      expect(JSON.parse(json)).toEqual(elements);
      expect(json).not.toMatch(/author|createdBy|provenance|owner/i);
    } finally {
      vi.useRealTimers();
    }
  });
});
