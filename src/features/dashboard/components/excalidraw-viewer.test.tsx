// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";

import { ExcalidrawViewer, parseExcalidrawScene } from "./excalidraw-viewer";

// Mock the lazy canvas chunk so the heavy `@excalidraw/excalidraw` bundle is
// never loaded in unit tests; expose what scene (and external-reload props) it
// was handed so we can assert the wrapper parsed/forwarded correctly.
vi.mock("./excalidraw-canvas", () => ({
  default: ({
    filePath,
    scene,
    externalScene,
    reloadNonce,
  }: {
    filePath: string;
    scene: { elements: unknown[] };
    externalScene: { elements: unknown[] } | null;
    reloadNonce: number;
  }) => (
    <div
      data-testid="excalidraw-canvas"
      data-filepath={filePath}
      data-elements={scene.elements.length}
      data-external-elements={externalScene ? externalScene.elements.length : "none"}
      data-reload-nonce={reloadNonce}
    >
      canvas
    </div>
  ),
}));

// The canvas load/persist only runs when a resolver context is present; pin
// it to `null` here so these wrapper tests stay focused on scene loading.
vi.mock("@/shared/hooks/use-resolver-context", () => ({
  useReadyResolverContext: () => null,
}));

const readFileMock = vi.fn<(path: string) => Promise<string>>();
vi.mock("@/tauri/commands", () => ({
  readFile: (path: string) => readFileMock(path),
}));

const toastErrorMock = vi.fn();
vi.mock("sonner", () => ({ toast: { error: (m: string) => toastErrorMock(m) } }));

const loggerDebugMock = vi.fn();
vi.mock("@/shared/lib/logger", () => ({
  logger: { debug: (...a: unknown[]) => loggerDebugMock(...a), error: vi.fn() },
}));

const VALID_SCENE = JSON.stringify({
  type: "excalidraw",
  version: 2,
  source: "local",
  elements: [
    { id: "a", type: "rectangle", x: 0, y: 0 },
    { id: "b", type: "arrow", x: 5, y: 5 },
  ],
  appState: { viewBackgroundColor: "#fff" },
  files: {},
});

// A distinct on-disk state (1 element) to represent an external write.
const ONE_ELEMENT_SCENE = JSON.stringify({
  type: "excalidraw",
  version: 2,
  source: "local",
  elements: [{ id: "z", type: "ellipse", x: 1, y: 1 }],
  appState: { viewBackgroundColor: "#fff" },
  files: {},
});

const BASE_PROPS = {
  filePath: "/x/design.excalidraw",
  teamContentPath: "/x",
} as const;

beforeEach(() => {
  readFileMock.mockReset();
  toastErrorMock.mockReset();
  loggerDebugMock.mockReset();
});

// AC-4 (round-trip building block) / AC-3 (empty + malformed handling).
describe("parseExcalidrawScene", () => {
  it("parses a valid scene preserving every element", () => {
    const scene = parseExcalidrawScene(VALID_SCENE);
    expect(scene.elements).toHaveLength(2);
    expect(scene.appState).toEqual({ viewBackgroundColor: "#fff" });
  });

  it("treats an empty / zero-byte file as a blank canvas", () => {
    expect(parseExcalidrawScene("")).toEqual({ elements: [], appState: {}, files: {} });
    expect(parseExcalidrawScene("   \n ")).toEqual({ elements: [], appState: {}, files: {} });
  });

  it("throws on malformed JSON (caller decides recovery)", () => {
    expect(() => parseExcalidrawScene("{not json")).toThrow();
  });

  it("throws on valid-JSON-but-not-a-scene-object (number/string/array)", () => {
    // Must route to the malformed path, not silently blank + overwrite.
    expect(() => parseExcalidrawScene("42")).toThrow();
    expect(() => parseExcalidrawScene('"hello"')).toThrow();
    expect(() => parseExcalidrawScene("null")).toThrow();
    expect(() => parseExcalidrawScene("[1,2,3]")).toThrow();
  });

  it("defends against a non-array elements field", () => {
    const scene = parseExcalidrawScene(JSON.stringify({ elements: "nope" }));
    expect(scene.elements).toEqual([]);
  });

  // SW-SC-4 AC-7 (openable scene): the render-safe skeleton an agent hand-authors
  // (per docs/shared-canvas-agent-authoring.md) must parse into an openable scene
  // with every element preserved — the precondition behind AC-1's file-path write.
  // This guards parse/open only; AC-1's actual agent file-write is agent-side (no
  // codesense code to unit-test), and real-engine RENDER fidelity is the manual/E2E
  // step in the loop runbook (a mocked engine can't assert it).
  it("parses an agent-authored render-safe skeleton into an openable scene (AC-7)", () => {
    const skeleton = JSON.stringify({
      type: "excalidraw",
      version: 2,
      source: "codesense",
      elements: [
        { id: "rect-1", type: "rectangle", x: 100, y: 100, width: 200, height: 120, index: "a0" },
        {
          id: "text-1",
          type: "text",
          x: 120,
          y: 150,
          width: 160,
          height: 25,
          index: "a1",
          text: "hi",
        },
        { id: "arrow-1", type: "arrow", x: 320, y: 160, width: 120, height: 0, index: "a2" },
      ],
      appState: { viewBackgroundColor: "#ffffff" },
      files: {},
    });
    const parsed = parseExcalidrawScene(skeleton);
    expect(parsed.elements).toHaveLength(3);
    expect(parsed.elements.map((e) => (e as { type: string }).type)).toEqual([
      "rectangle",
      "text",
      "arrow",
    ]);
    expect(parsed.appState).toEqual({ viewBackgroundColor: "#ffffff" });
  });
});

describe("ExcalidrawViewer", () => {
  it("loads a file's scene and mounts the canvas with its elements (AC-3)", async () => {
    readFileMock.mockResolvedValue(VALID_SCENE);
    render(<ExcalidrawViewer filePath="/x/design.excalidraw" teamContentPath="/x" />);

    const canvas = await screen.findByTestId("excalidraw-canvas");
    expect(canvas).toHaveAttribute("data-filepath", "/x/design.excalidraw");
    expect(canvas).toHaveAttribute("data-elements", "2");
    expect(readFileMock).toHaveBeenCalledWith("/x/design.excalidraw");
  });

  it("opens a blank canvas + nudges on malformed JSON, never crashing (AC-3)", async () => {
    readFileMock.mockResolvedValue("{ totally broken");
    render(<ExcalidrawViewer filePath="/x/broken.excalidraw" teamContentPath="/x" />);

    const canvas = await screen.findByTestId("excalidraw-canvas");
    expect(canvas).toHaveAttribute("data-elements", "0");
    expect(toastErrorMock).toHaveBeenCalledTimes(1);
  });

  it("opens a blank canvas for a new / unreadable file (AC-3)", async () => {
    readFileMock.mockRejectedValue(new Error("ENOENT"));
    render(<ExcalidrawViewer filePath="/x/new.excalidraw" teamContentPath="/x" />);

    const canvas = await screen.findByTestId("excalidraw-canvas");
    expect(canvas).toHaveAttribute("data-elements", "0");
    expect(toastErrorMock).not.toHaveBeenCalled();
  });

  it("renders a placeholder when no file is selected", () => {
    render(<ExcalidrawViewer filePath={null} teamContentPath="/x" />);
    expect(screen.getByText(/select a file to view/i)).toBeInTheDocument();
    expect(readFileMock).not.toHaveBeenCalled();
  });
});

// SW-SC-3: the wrapper re-reads and forwards an external scene only when THIS
// file actually changes on disk, and never clobbers on a malformed write.
describe("ExcalidrawViewer hot-reload gate", () => {
  it("re-reads and forwards a new external scene when this file changes (AC-1/AC-2)", async () => {
    readFileMock.mockResolvedValue(VALID_SCENE);
    const { rerender } = render(
      <ExcalidrawViewer {...BASE_PROPS} refreshKey={1} changedPaths={null} />,
    );
    const canvas = await screen.findByTestId("excalidraw-canvas");
    expect(canvas).toHaveAttribute("data-external-elements", "none");
    expect(canvas).toHaveAttribute("data-reload-nonce", "0");
    expect(readFileMock).toHaveBeenCalledTimes(1);

    // External write: this file is in the changed set; new content has 1 element.
    readFileMock.mockResolvedValue(ONE_ELEMENT_SCENE);
    rerender(
      <ExcalidrawViewer
        {...BASE_PROPS}
        refreshKey={2}
        changedPaths={new Set([BASE_PROPS.filePath])}
      />,
    );

    await waitFor(() =>
      expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute(
        "data-external-elements",
        "1",
      ),
    );
    expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute("data-reload-nonce", "1");
    expect(readFileMock).toHaveBeenCalledTimes(2);
  });

  // SW-SC-11: the agent saves atomically (write `<file>.tmp.<pid>.<hash>` →
  // rename). The temp sibling must NOT reload the canvas (its path can't match
  // the per-file gate); the real rename destination — which the Rust emit now
  // forwards after filtering the temp — does. End-to-end this is what makes
  // "watch the agent's shapes appear" work for a real atomic writer.
  it("ignores an atomic-temp sibling and reloads on the real rename target (SW-SC-11)", async () => {
    readFileMock.mockResolvedValue(VALID_SCENE);
    const { rerender } = render(
      <ExcalidrawViewer {...BASE_PROPS} refreshKey={1} changedPaths={null} />,
    );
    await screen.findByTestId("excalidraw-canvas");
    expect(readFileMock).toHaveBeenCalledTimes(1);

    // Temp sibling in the changed set → gate holds, no re-read.
    readFileMock.mockResolvedValue(ONE_ELEMENT_SCENE);
    rerender(
      <ExcalidrawViewer
        {...BASE_PROPS}
        refreshKey={2}
        changedPaths={new Set([`${BASE_PROPS.filePath}.tmp.99999.deadbeef`])}
      />,
    );
    await new Promise((r) => setTimeout(r, 0));
    expect(readFileMock).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute(
      "data-external-elements",
      "none",
    );

    // Real rename destination in the set → reload.
    rerender(
      <ExcalidrawViewer
        {...BASE_PROPS}
        refreshKey={3}
        changedPaths={new Set([BASE_PROPS.filePath])}
      />,
    );
    await waitFor(() =>
      expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute(
        "data-external-elements",
        "1",
      ),
    );
    expect(readFileMock).toHaveBeenCalledTimes(2);
  });

  it("does NOT re-read when a refresh bump excludes this file's path (AC-1 gate)", async () => {
    readFileMock.mockResolvedValue(VALID_SCENE);
    const { rerender } = render(
      <ExcalidrawViewer {...BASE_PROPS} refreshKey={1} changedPaths={null} />,
    );
    await screen.findByTestId("excalidraw-canvas");
    expect(readFileMock).toHaveBeenCalledTimes(1);

    rerender(
      <ExcalidrawViewer
        {...BASE_PROPS}
        refreshKey={2}
        changedPaths={new Set(["/x/other.excalidraw"])}
      />,
    );
    // Let any (incorrectly-firing) effect run before asserting it didn't.
    await new Promise((r) => setTimeout(r, 0));
    expect(readFileMock).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute(
      "data-external-elements",
      "none",
    );
  });

  // SW-SC-4 AC-7: an unparseable external write (e.g. a watcher tick that read
  // a truncated file mid-write) must keep the open canvas AND stay silent —
  // it self-heals on the settle event, so a toast per partial read would be
  // noise. Contrast with the INITIAL open of a broken file, which still toasts.
  it("keeps the current canvas and self-heals silently on a malformed external write (AC-7)", async () => {
    readFileMock.mockResolvedValue(VALID_SCENE);
    const { rerender } = render(
      <ExcalidrawViewer {...BASE_PROPS} refreshKey={1} changedPaths={null} />,
    );
    await screen.findByTestId("excalidraw-canvas");
    expect(readFileMock).toHaveBeenCalledTimes(1);

    readFileMock.mockResolvedValue("{ broken json");
    rerender(
      <ExcalidrawViewer
        {...BASE_PROPS}
        refreshKey={2}
        changedPaths={new Set([BASE_PROPS.filePath])}
      />,
    );

    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    // No user-facing toast for a transient/partial write — logged instead.
    expect(toastErrorMock).not.toHaveBeenCalled();
    expect(loggerDebugMock).toHaveBeenCalledWith(
      "viewer",
      expect.stringContaining("reload skipped"),
    );
    // The malformed read must not advance the external scene (no clobber).
    expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute(
      "data-external-elements",
      "none",
    );
    expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute("data-reload-nonce", "0");
  });

  // SW-SC-4 AC-7 (Gap B): a non-atomic agent write can surface as two ticks —
  // a truncated/partial read then the complete file. The loop must CONVERGE on
  // the complete scene: the partial read is dropped (canvas intact), the settle
  // read applies. End state is always the complete write, never corruption.
  it("converges on the complete scene across a truncate→complete write sequence (AC-7)", async () => {
    readFileMock.mockResolvedValue(VALID_SCENE);
    const { rerender } = render(
      <ExcalidrawViewer {...BASE_PROPS} refreshKey={1} changedPaths={null} />,
    );
    await screen.findByTestId("excalidraw-canvas");

    // Tick 1: watcher fired mid-write → truncated/unparseable read → dropped.
    readFileMock.mockResolvedValue('{ "type": "excalidraw", "elements": [');
    rerender(
      <ExcalidrawViewer
        {...BASE_PROPS}
        refreshKey={2}
        changedPaths={new Set([BASE_PROPS.filePath])}
      />,
    );
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute("data-reload-nonce", "0");

    // Tick 2 (settle): the complete file lands → applies.
    readFileMock.mockResolvedValue(ONE_ELEMENT_SCENE);
    rerender(
      <ExcalidrawViewer
        {...BASE_PROPS}
        refreshKey={3}
        changedPaths={new Set([BASE_PROPS.filePath])}
      />,
    );
    await waitFor(() =>
      expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute(
        "data-external-elements",
        "1",
      ),
    );
    expect(screen.getByTestId("excalidraw-canvas")).toHaveAttribute("data-reload-nonce", "1");
    expect(toastErrorMock).not.toHaveBeenCalled();
  });
});
