// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import {
  exportCanvas,
  exportDefaultFilename,
  renderCanvasToPngBase64,
  type CanvasExportEngine,
  type ExportCanvasDeps,
} from "@/features/dashboard/lib/export-canvas";

const SCENE_JSON = JSON.stringify({
  type: "excalidraw",
  elements: [{ id: "a", type: "rectangle" }],
  appState: { viewBackgroundColor: "#fff" },
  files: {},
});

// Non-ASCII marker baked into the serialized SVG so the encoding assertion
// actually exercises `utf8ToBase64` — plain `btoa` throws on U+2192 (→).
const NON_ASCII = "café→";

function base64ToBytes(b64: string): Uint8Array {
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}
function base64ToUtf8(b64: string): string {
  return new TextDecoder().decode(base64ToBytes(b64));
}

function makeEngine(): CanvasExportEngine & {
  exportToBlob: ReturnType<typeof vi.fn>;
  exportToSvg: ReturnType<typeof vi.fn>;
} {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("data-label", NON_ASCII);
  return {
    exportToBlob: vi.fn(async () => new Blob([new Uint8Array([1, 2, 3])], { type: "image/png" })),
    exportToSvg: vi.fn(async () => svg),
  };
}

function makeDeps(overrides: Partial<ExportCanvasDeps> = {}): {
  deps: ExportCanvasDeps;
  engine: ReturnType<typeof makeEngine>;
  loadEngine: ReturnType<typeof vi.fn>;
  save: ReturnType<typeof vi.fn>;
  writeBinaryFile: ReturnType<typeof vi.fn>;
  toast: {
    loading: ReturnType<typeof vi.fn>;
    success: ReturnType<typeof vi.fn>;
    error: ReturnType<typeof vi.fn>;
  };
} {
  const engine = makeEngine();
  const loadEngine = vi.fn(async () => engine);
  const save = vi.fn(async () => "/out/design.png");
  const writeBinaryFile = vi.fn(async () => {});
  const toast = {
    loading: vi.fn(() => "toast-id"),
    success: vi.fn(),
    error: vi.fn(),
  };
  const deps: ExportCanvasDeps = {
    readFile: vi.fn(async () => SCENE_JSON),
    save,
    loadEngine,
    writeBinaryFile,
    toast,
    ...overrides,
  };
  return { deps, engine, loadEngine, save, writeBinaryFile, toast };
}

describe("exportDefaultFilename (AC-4)", () => {
  it("swaps .excalidraw for the export extension", () => {
    expect(exportDefaultFilename("/x/y/design.excalidraw", "png")).toBe("design.png");
    expect(exportDefaultFilename("/x/y/design.excalidraw", "svg")).toBe("design.svg");
    expect(exportDefaultFilename("flat.excalidraw", "png")).toBe("flat.png");
  });
});

describe("exportCanvas", () => {
  beforeEach(() => vi.clearAllMocks());

  it("AC-1: PNG export renders via exportToBlob and writes the chosen path", async () => {
    const { deps, engine, save, writeBinaryFile, toast } = makeDeps();
    await exportCanvas("/x/design.excalidraw", "png", deps);

    expect(save).toHaveBeenCalledWith({
      defaultPath: "design.png",
      filters: [{ name: "PNG Image", extensions: ["png"] }],
    });
    expect(engine.exportToBlob).toHaveBeenCalledTimes(1);
    expect(engine.exportToBlob.mock.calls[0][0]).toMatchObject({ mimeType: "image/png" });
    expect(writeBinaryFile).toHaveBeenCalledTimes(1);
    expect(writeBinaryFile.mock.calls[0][1]).toBe("/out/design.png");
    // the written base64 decodes back to the exact rendered PNG bytes
    expect(Array.from(base64ToBytes(writeBinaryFile.mock.calls[0][0] as string))).toEqual([
      1, 2, 3,
    ]);
    expect(toast.success).toHaveBeenCalled();
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("AC-2: SVG export serializes exportToSvg output and writes it", async () => {
    const { deps, engine, save, writeBinaryFile } = makeDeps();
    await exportCanvas("/x/design.excalidraw", "svg", deps);

    expect(save).toHaveBeenCalledWith({
      defaultPath: "design.svg",
      filters: [{ name: "SVG Image", extensions: ["svg"] }],
    });
    expect(engine.exportToSvg).toHaveBeenCalledTimes(1);
    expect(engine.exportToBlob).not.toHaveBeenCalled();
    expect(writeBinaryFile).toHaveBeenCalledTimes(1);
    // utf-8 base64 of the serialized markup — round-trips through TextDecoder.
    const decoded = base64ToUtf8(writeBinaryFile.mock.calls[0][0] as string);
    expect(decoded).toContain("<svg");
    // The non-ASCII marker survives — proves `utf8ToBase64`, not plain `btoa`
    // (which would throw on U+2192), is what serialized the SVG.
    expect(decoded).toContain(NON_ASCII);
  });

  it("AC-3/AC-8: cancelling the save dialog writes nothing and never loads the engine", async () => {
    const { deps, loadEngine, writeBinaryFile, toast } = makeDeps({
      save: vi.fn(async () => null),
    });
    await exportCanvas("/x/design.excalidraw", "png", deps);

    expect(loadEngine).not.toHaveBeenCalled();
    expect(writeBinaryFile).not.toHaveBeenCalled();
    expect(toast.loading).not.toHaveBeenCalled();
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("AC-8: the save dialog is shown before the engine is loaded", async () => {
    const calls: string[] = [];
    const { deps } = makeDeps({
      save: vi.fn(async () => {
        calls.push("save");
        return "/out/design.png";
      }),
      loadEngine: vi.fn(async () => {
        calls.push("loadEngine");
        return makeEngine();
      }),
    });
    await exportCanvas("/x/design.excalidraw", "png", deps);
    expect(calls).toEqual(["save", "loadEngine"]);
  });

  it("malformed canvas → error toast, no engine load, no write", async () => {
    const { deps, loadEngine, writeBinaryFile, toast } = makeDeps({
      readFile: vi.fn(async () => "{ not json"),
    });
    await exportCanvas("/x/design.excalidraw", "png", deps);

    expect(loadEngine).not.toHaveBeenCalled();
    expect(writeBinaryFile).not.toHaveBeenCalled();
    expect(toast.error).toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("a degenerate (empty) render surfaces an error and writes nothing", async () => {
    const { deps, writeBinaryFile, toast } = makeDeps({
      loadEngine: vi.fn(
        async (): Promise<CanvasExportEngine> => ({
          exportToBlob: async () => new Blob([], { type: "image/png" }),
          exportToSvg: async () => document.createElementNS("http://www.w3.org/2000/svg", "svg"),
        }),
      ),
    });
    await exportCanvas("/x/design.excalidraw", "png", deps);

    expect(writeBinaryFile).not.toHaveBeenCalled();
    expect(toast.error).toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("a write failure surfaces an error toast (not a throw)", async () => {
    const { deps, toast } = makeDeps({
      writeBinaryFile: vi.fn(async () => {
        throw new Error("disk full");
      }),
    });
    await expect(exportCanvas("/x/design.excalidraw", "png", deps)).resolves.toBeUndefined();
    expect(toast.error).toHaveBeenCalled();
  });
});

describe("renderCanvasToPngBase64 (SW-SC-14)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("reads the file fresh and renders PNG bytes through exportToBlob", async () => {
    const engine = makeEngine();
    const readFile = vi.fn(async () => SCENE_JSON);
    const loadEngine = vi.fn(async () => engine);

    const base64 = await renderCanvasToPngBase64("/x/design.excalidraw", { readFile, loadEngine });

    expect(readFile).toHaveBeenCalledWith("/x/design.excalidraw");
    expect(engine.exportToBlob).toHaveBeenCalledTimes(1);
    expect(engine.exportToBlob.mock.calls[0][0]).toMatchObject({ mimeType: "image/png" });
    // base64 decodes back to the rendered PNG bytes — never the SVG path.
    expect(Array.from(base64ToBytes(base64))).toEqual([1, 2, 3]);
    expect(engine.exportToSvg).not.toHaveBeenCalled();
  });

  it("throws on a degenerate (empty) render so the bridge resolves with an error", async () => {
    const readFile = vi.fn(async () => SCENE_JSON);
    const loadEngine = vi.fn(
      async (): Promise<CanvasExportEngine> => ({
        exportToBlob: async () => new Blob([], { type: "image/png" }),
        exportToSvg: async () => document.createElementNS("http://www.w3.org/2000/svg", "svg"),
      }),
    );
    await expect(
      renderCanvasToPngBase64("/x/design.excalidraw", { readFile, loadEngine }),
    ).rejects.toThrow(/no image data/);
  });

  it("propagates a malformed-scene parse error (never loads the engine)", async () => {
    const readFile = vi.fn(async () => "{ not json");
    const loadEngine = vi.fn(async () => makeEngine());
    await expect(
      renderCanvasToPngBase64("/x/design.excalidraw", { readFile, loadEngine }),
    ).rejects.toBeInstanceOf(Error);
    expect(loadEngine).not.toHaveBeenCalled();
  });
});

it("renders admitted backend scene bytes without reading under the active team", async () => {
  const { deps } = makeDeps({
    readFile: vi.fn(async () => {
      throw new Error("must not re-read");
    }),
  });
  expect(await renderCanvasToPngBase64("/parent/shared.excalidraw", deps, SCENE_JSON)).toBeTruthy();
  expect(deps.readFile).not.toHaveBeenCalled();
});
