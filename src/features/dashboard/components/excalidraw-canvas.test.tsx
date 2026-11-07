// SPDX-License-Identifier: MIT
import fs from "node:fs";
import path from "node:path";
import { vi, describe, it, expect, beforeEach } from "vitest";
import { render, waitFor } from "@testing-library/react";

// `canvasPngPath` moved to the engine-free `viewer-file-type` leaf (SW-SC-14);
// its unit test lives in `viewer-file-type.test.ts`.

// --- Component behavior ---
//
// The heavy `@excalidraw/excalidraw` chunk and the debounce are stubbed so the
// tests are deterministic: the mock Excalidraw fires `onChange` on demand (PNG
// export, SW-SC-2) and hands back a fake imperative API with `updateLibrary`
// (team-library load, SW-SC-8).

const writeContentFile = vi.fn<(p: string, c: string) => Promise<void>>().mockResolvedValue();
const writeBinaryContentFile = vi.fn<(p: string, b: string) => Promise<void>>().mockResolvedValue();
// Capture the `updateLibrary` calls the injection effect makes.
const h = vi.hoisted(() => ({ updateLibrary: vi.fn() }));
const readMock = vi.fn<() => Promise<string | null>>();
const writeMock = vi.fn<(p: unknown) => Promise<void>>().mockResolvedValue();
vi.mock("@/tauri/commands", () => ({
  writeContentFile: (p: string, c: string) => writeContentFile(p, c),
  writeBinaryContentFile: (p: string, b: string) => writeBinaryContentFile(p, b),
  readCanvasLibrary: () => readMock(),
  writeCanvasLibrary: (p: unknown) => writeMock(p),
}));

// Run debounced actions synchronously; expose flush/cancel as no-ops.
vi.mock("@/shared/lib/debounced-action", () => ({
  createDebouncedAction: (fn: (...a: unknown[]) => unknown) => ({
    trigger: (...a: unknown[]) => fn(...a),
    flush: () => {},
    cancel: () => {},
  }),
}));

vi.mock("next-themes", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));
vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

let fireScene: (els: unknown[]) => void = () => {};
let fireLibrary: (items: { id: string }[]) => void = () => {};
let validateEmbeddable: ((link: string) => boolean | undefined) | undefined;
vi.mock("@excalidraw/excalidraw", async () => {
  const React = await vi.importActual<typeof import("react")>("react");
  return {
    Excalidraw: ({
      onChange,
      onLibraryChange,
      excalidrawAPI,
      validateEmbeddable: validate,
    }: {
      onChange?: (e: unknown[], a: unknown, f: unknown) => void;
      onLibraryChange?: (items: { id: string }[]) => void;
      excalidrawAPI?: (api: unknown) => void;
      validateEmbeddable?: (link: string) => boolean | undefined;
    }) => {
      validateEmbeddable = validate;
      fireScene = (els) => onChange?.(els, { viewBackgroundColor: "#fff" }, {});
      fireLibrary = (items) => onLibraryChange?.(items);
      // Hand the canvas a fake imperative API once mounted (via effect, so we
      // never setState during render).
      React.useEffect(() => {
        excalidrawAPI?.({ updateLibrary: h.updateLibrary });
      }, [excalidrawAPI]);
      return React.createElement("div", { "data-testid": "excalidraw" });
    },
    serializeAsJSON: () => '{"json":true}',
    hashElementsVersion: (els: unknown[]) => els.length,
    exportToBlob: async () => new Blob([new Uint8Array([1, 2, 3])], { type: "image/png" }),
    CaptureUpdateAction: { NEVER: "never" },
  };
});

import ExcalidrawCanvas from "./excalidraw-canvas";

const EMPTY_SCENE = { elements: [], appState: {}, files: {} };
const CTX = { orgSlug: "acme/api", teamName: "default", codesenseHome: "/home", userId: 42 };

beforeEach(() => {
  writeContentFile.mockClear();
  writeBinaryContentFile.mockClear();
  writeMock.mockClear().mockResolvedValue();
  h.updateLibrary.mockReset().mockResolvedValue([{ id: "team-item" }]);
  readMock.mockReset().mockResolvedValue(null);
});

describe("ExcalidrawCanvas PNG export (SW-SC-2)", () => {
  it("writes the JSON and a derived PNG on a real edit", async () => {
    render(<ExcalidrawCanvas filePath="/x/d.excalidraw" scene={EMPTY_SCENE} libraryCtx={null} />);
    // A genuine edit: two elements where the seed had zero → signature moves.
    fireScene([{ id: "a" }, { id: "b" }]);

    expect(writeContentFile).toHaveBeenCalledWith("/x/d.excalidraw", '{"json":true}');
    await waitFor(() =>
      expect(writeBinaryContentFile).toHaveBeenCalledWith(
        "/x/d.excalidraw.png",
        expect.any(String),
      ),
    );
  });

  it("does not write when the persisted signature is unchanged", () => {
    // Seed matches exactly what the mock Excalidraw fires (0 elements,
    // viewBackgroundColor #fff) → signature unmoved → no JSON/PNG write.
    render(
      <ExcalidrawCanvas
        filePath="/x/d.excalidraw"
        scene={{ elements: [], appState: { viewBackgroundColor: "#fff" }, files: {} }}
        libraryCtx={null}
      />,
    );
    fireScene([]);
    expect(writeContentFile).not.toHaveBeenCalled();
    expect(writeBinaryContentFile).not.toHaveBeenCalled();
  });
});

describe("ExcalidrawCanvas team-library load (SW-SC-8, AC-2/3)", () => {
  it("injects the team bag once with merge:false on mount", async () => {
    readMock.mockResolvedValue('{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"a"}]}');

    render(<ExcalidrawCanvas filePath="/x.excalidraw" scene={EMPTY_SCENE} libraryCtx={CTX} />);

    await waitFor(() => expect(h.updateLibrary).toHaveBeenCalledTimes(1));
    // The whole bag replaces the panel (merge:false); a Blob is handed to
    // Excalidraw's own loader (we never parse the envelope here).
    const opts = h.updateLibrary.mock.calls[0][0];
    expect(opts).toMatchObject({ merge: false });
    expect(opts.libraryItems).toBeInstanceOf(Blob);
  });

  it("injects nothing when the team has no bag yet (read → null)", async () => {
    readMock.mockResolvedValue(null);
    render(<ExcalidrawCanvas filePath="/x.excalidraw" scene={EMPTY_SCENE} libraryCtx={CTX} />);
    await new Promise((r) => setTimeout(r, 0));
    expect(h.updateLibrary).not.toHaveBeenCalled();
  });

  it("does nothing without a resolved team context (D5)", async () => {
    render(<ExcalidrawCanvas filePath="/x.excalidraw" scene={EMPTY_SCENE} libraryCtx={null} />);
    await new Promise((r) => setTimeout(r, 0));
    expect(readMock).not.toHaveBeenCalled();
    expect(h.updateLibrary).not.toHaveBeenCalled();
  });
});

describe("ExcalidrawCanvas team-library persist (SW-SC-8, AC-3)", () => {
  it("persists a real library change but suppresses an unchanged re-fire", async () => {
    readMock.mockResolvedValue(null); // nothing to inject; injecting gate stays down
    render(<ExcalidrawCanvas filePath="/x.excalidraw" scene={EMPTY_SCENE} libraryCtx={CTX} />);
    await new Promise((r) => setTimeout(r, 0));

    fireLibrary([{ id: "a" }]);
    fireLibrary([{ id: "a" }]); // identical bag → no-op re-fire, suppressed
    expect(writeMock).toHaveBeenCalledTimes(1);
    const persisted = writeMock.mock.calls[0][0] as { content: string };
    expect(JSON.parse(persisted.content).libraryItems).toHaveLength(1);

    fireLibrary([{ id: "a" }, { id: "b" }]); // a genuine add → persists again
    expect(writeMock).toHaveBeenCalledTimes(2);
  });

  it("does not persist a library change without a team (D5)", async () => {
    render(<ExcalidrawCanvas filePath="/x.excalidraw" scene={EMPTY_SCENE} libraryCtx={null} />);
    await new Promise((r) => setTimeout(r, 0));
    fireLibrary([{ id: "a" }]);
    expect(writeMock).not.toHaveBeenCalled();
  });
});

describe("ExcalidrawCanvas outside hosts", () => {
  it("embeds no web link, so a canvas opens no third-party frame", () => {
    render(<ExcalidrawCanvas filePath="/x.excalidraw" scene={EMPTY_SCENE} libraryCtx={null} />);
    // Excalidraw takes a boolean from the function as final; its built-in allowlist never runs.
    for (const link of [
      "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
      "https://www.figma.com/file/abc",
      "https://x.com/someone/status/1",
      "https://gist.github.com/someone/abc",
    ]) {
      expect(validateEmbeddable?.(link)).toBe(false);
    }
  });

  it("hides the library's Publish item, which posts to Excalidraw's cloud function", () => {
    const { container } = render(
      <ExcalidrawCanvas filePath="/x.excalidraw" scene={EMPTY_SCENE} libraryCtx={null} />,
    );
    const css = container.querySelector("style")?.textContent ?? "";
    expect(css).toContain('.library-menu [data-testid="lib-dropdown--remove"] { display: none; }');
  });

  it("the installed Excalidraw still gives Publish, and only Publish, that menu id", () => {
    // The hide rule matches Excalidraw's markup; an upgrade that renames it must fail here.
    const bundle = fs.readFileSync(
      path.resolve(__dirname, "../../../../node_modules/@excalidraw/excalidraw/dist/prod/index.js"),
      "utf8",
    );
    expect(bundle).toContain('className:"library-menu"');
    expect(bundle.match(/"data-testid":"lib-dropdown--remove"/g)).toHaveLength(1);
    expect(bundle).toMatch(
      /"data-testid":"lib-dropdown--remove",children:[\w$]+\("buttons\.publishLibrary"\)/,
    );
  });
});
