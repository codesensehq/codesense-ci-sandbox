// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import {
  canCreateCanvas,
  canExportCanvas,
  canvasCandidatePath,
  isFileExistsError,
  createCanvasForDoc,
  type CreateCanvasDeps,
} from "@/features/dashboard/lib/create-canvas";

describe("canCreateCanvas (AC-1, AC-2 scope gate)", () => {
  it("is true for markdown / html doc nodes (story AND plain context docs)", () => {
    expect(canCreateCanvas("/x/y/architecture.md")).toBe(true);
    expect(canCreateCanvas("/x/y/ux.html")).toBe(true);
    expect(canCreateCanvas("notes.htm")).toBe(true);
    // uppercase extension still gates true
    expect(canCreateCanvas("/x/README.MD")).toBe(true);
  });

  it("is false for canvases, images, and the derived *.excalidraw.png render", () => {
    expect(canCreateCanvas("/x/drawing.excalidraw")).toBe(false);
    expect(canCreateCanvas("/x/photo.png")).toBe(false);
    expect(canCreateCanvas("/x/logo.svg")).toBe(false);
    // derived render: png ext already excludes it; isDerivedRenderArtifact is belt-and-suspenders
    expect(canCreateCanvas("/x/diagram.excalidraw.png")).toBe(false);
  });

  it("is false for folder / extensionless / generic non-viewable nodes", () => {
    expect(canCreateCanvas("/x/some-folder")).toBe(false);
    expect(canCreateCanvas("/x/data.json")).toBe(false);
    expect(canCreateCanvas("/x/notes.txt")).toBe(false);
  });
});

describe("canExportCanvas (SW-SC-13 AC-9 — shared canvas-node gate)", () => {
  it("is true only for .excalidraw canvas nodes", () => {
    expect(canExportCanvas("/x/y/design.excalidraw")).toBe(true);
    expect(canExportCanvas("design.excalidraw")).toBe(true);
  });

  it("is false for the derived *.excalidraw.png render (resolves to image)", () => {
    expect(canExportCanvas("/x/design.excalidraw.png")).toBe(false);
    expect(canExportCanvas("/x/DESIGN.EXCALIDRAW.PNG")).toBe(false);
  });

  it("is false for docs, images, folders, and generic nodes", () => {
    expect(canExportCanvas("/x/architecture.md")).toBe(false);
    expect(canExportCanvas("/x/ux.html")).toBe(false);
    expect(canExportCanvas("/x/photo.png")).toBe(false);
    expect(canExportCanvas("/x/some-folder")).toBe(false);
    expect(canExportCanvas("/x/data.json")).toBe(false);
  });
});

describe("canvasCandidatePath (AC-3 naming + collision suffix)", () => {
  it("derives a flat sibling .excalidraw named after the doc (attempt 0)", () => {
    expect(canvasCandidatePath("/x/y/architecture.md", 0)).toBe("/x/y/architecture.excalidraw");
    expect(canvasCandidatePath("/x/ux.html", 0)).toBe("/x/ux.excalidraw");
  });

  it("suffixes -2, -3, … on subsequent attempts", () => {
    expect(canvasCandidatePath("/x/y/architecture.md", 1)).toBe("/x/y/architecture-2.excalidraw");
    expect(canvasCandidatePath("/x/y/architecture.md", 2)).toBe("/x/y/architecture-3.excalidraw");
  });

  it("handles a bare filename with no directory", () => {
    expect(canvasCandidatePath("readme.md", 0)).toBe("readme.excalidraw");
    expect(canvasCandidatePath("readme.md", 1)).toBe("readme-2.excalidraw");
  });
});

describe("isFileExistsError (Finding B — drives FE collision-suffix)", () => {
  it("recognizes create_file's error-on-exist string", () => {
    expect(isFileExistsError("File already exists: /x/a.excalidraw")).toBe(true);
    expect(isFileExistsError(new Error("File already exists: /x/a.excalidraw"))).toBe(true);
  });

  it("does not match other failures", () => {
    expect(isFileExistsError("Permission denied")).toBe(false);
    expect(isFileExistsError(new Error("disk full"))).toBe(false);
  });

  it("does not misread a different 'already exists' error as a collision", () => {
    // Only `create_file`'s exact "File already exists" contract counts — a
    // lower-layer "directory already exists" must abort, not silently retry.
    expect(isFileExistsError("directory already exists")).toBe(false);
  });
});

function makeDeps(over: Partial<CreateCanvasDeps> = {}): {
  deps: CreateCanvasDeps;
  createFile: ReturnType<typeof vi.fn>;
  readFile: ReturnType<typeof vi.fn>;
  writeContentFile: ReturnType<typeof vi.fn>;
  onOpen: ReturnType<typeof vi.fn>;
} {
  const createFile = vi.fn(async () => {});
  const readFile = vi.fn(async () => "# Doc\n");
  const writeContentFile = vi.fn(async () => {});
  const onOpen = vi.fn();
  const deps: CreateCanvasDeps = { createFile, readFile, writeContentFile, onOpen, ...over };
  return { deps, createFile, readFile, writeContentFile, onOpen };
}

describe("createCanvasForDoc (AC-3, AC-4, AC-5 composition)", () => {
  it("creates a blank sibling, opens it, and attaches it to the doc frontmatter", async () => {
    const { deps, createFile, writeContentFile, onOpen } = makeDeps();
    const result = await createCanvasForDoc("/x/architecture.md", deps);

    expect(result).toEqual({
      ok: true,
      canvasPath: "/x/architecture.excalidraw",
      attach: "written",
    });
    // blank file, flat sibling
    expect(createFile).toHaveBeenCalledWith("/x/architecture.excalidraw", "");
    // opened (AC-5)
    expect(onOpen).toHaveBeenCalledWith("/x/architecture.excalidraw");
    // attached: frontmatter written because the ref was newly added (AC-4)
    expect(writeContentFile).toHaveBeenCalledTimes(1);
    expect(writeContentFile.mock.calls[0][0]).toBe("/x/architecture.md");
    expect(writeContentFile.mock.calls[0][1]).toContain("./architecture.excalidraw");
  });

  it("opens the canvas BEFORE writing the frontmatter (non-blocking order)", async () => {
    const order: string[] = [];
    const { deps } = makeDeps({
      onOpen: vi.fn(() => order.push("open")),
      writeContentFile: vi.fn(async () => {
        order.push("write");
      }),
    });
    await createCanvasForDoc("/x/architecture.md", deps);
    expect(order).toEqual(["open", "write"]);
  });

  it("auto-suffixes on a name collision and never overwrites (AC-3)", async () => {
    const createFile = vi.fn(async (path: string) => {
      if (path === "/x/architecture.excalidraw") {
        throw "File already exists: /x/architecture.excalidraw";
      }
    });
    const { deps, onOpen } = makeDeps({ createFile });
    const result = await createCanvasForDoc("/x/architecture.md", deps);

    expect(result).toMatchObject({ ok: true, canvasPath: "/x/architecture-2.excalidraw" });
    expect(createFile).toHaveBeenNthCalledWith(1, "/x/architecture.excalidraw", "");
    expect(createFile).toHaveBeenNthCalledWith(2, "/x/architecture-2.excalidraw", "");
    expect(onOpen).toHaveBeenCalledWith("/x/architecture-2.excalidraw");
  });

  it("gives up with name-exhausted after every candidate collides (no open)", async () => {
    const createFile = vi.fn(async (path: string) => {
      throw `File already exists: ${path}`;
    });
    const { deps, onOpen, writeContentFile } = makeDeps({ createFile });
    const result = await createCanvasForDoc("/x/architecture.md", deps);

    expect(result).toEqual({ ok: false, reason: "name-exhausted" });
    // bare name + suffixes -2..-99 = 99 distinct candidates, all probed once
    expect(createFile).toHaveBeenCalledTimes(99);
    expect(createFile).toHaveBeenNthCalledWith(1, "/x/architecture.excalidraw", "");
    expect(createFile).toHaveBeenNthCalledWith(99, "/x/architecture-99.excalidraw", "");
    expect(onOpen).not.toHaveBeenCalled();
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("does not become an unhandled rejection if the open handler throws", async () => {
    const onOpen = vi.fn(() => {
      throw new Error("viewer store blew up");
    });
    const { deps, writeContentFile } = makeDeps({ onOpen });
    // Must resolve (not reject) — and the attach must still run despite the throw.
    const result = await createCanvasForDoc("/x/architecture.md", deps);

    expect(result).toEqual({
      ok: true,
      canvasPath: "/x/architecture.excalidraw",
      attach: "written",
    });
    expect(onOpen).toHaveBeenCalledWith("/x/architecture.excalidraw");
    expect(writeContentFile).toHaveBeenCalledTimes(1);
  });

  it("aborts (no open, no attach) on a non-collision create failure", async () => {
    const createFile = vi.fn(async () => {
      throw "Permission denied";
    });
    const { deps, onOpen, writeContentFile } = makeDeps({ createFile });
    const result = await createCanvasForDoc("/x/architecture.md", deps);

    expect(result).toEqual({ ok: false, reason: "create-failed" });
    expect(onOpen).not.toHaveBeenCalled();
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("treats attach failure as non-fatal — canvas is still created and opened", async () => {
    const readFile = vi.fn(async () => {
      throw "read blew up";
    });
    const { deps, onOpen } = makeDeps({ readFile });
    const result = await createCanvasForDoc("/x/architecture.md", deps);

    expect(result).toEqual({
      ok: true,
      canvasPath: "/x/architecture.excalidraw",
      attach: "failed",
    });
    expect(onOpen).toHaveBeenCalledWith("/x/architecture.excalidraw");
  });

  it("does not rewrite frontmatter when the canvas is already attached (idempotent)", async () => {
    const readFile = vi.fn(
      async () => "---\nsupporting_docs:\n  - ./architecture.excalidraw\n---\n# Arch\n",
    );
    const { deps, writeContentFile } = makeDeps({ readFile });
    const result = await createCanvasForDoc("/x/architecture.md", deps);

    expect(result).toEqual({
      ok: true,
      canvasPath: "/x/architecture.excalidraw",
      attach: "already",
    });
    expect(writeContentFile).not.toHaveBeenCalled();
  });
});
