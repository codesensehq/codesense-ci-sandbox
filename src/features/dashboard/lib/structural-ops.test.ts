// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import {
  baseName,
  computeChildTarget,
  computeRenameTarget,
  createContent,
  createContentUnique,
  deleteContent,
  ingestFile,
  ingestFileUnique,
  moveContentTo,
  renameContent,
  sanitizeSegment,
  suffixName,
} from "./structural-ops";

describe("baseName", () => {
  it("returns the last path segment", () => {
    expect(baseName("/a/b/c.md")).toBe("c.md");
    expect(baseName("/a/b/")).toBe("b");
    expect(baseName("solo.md")).toBe("solo.md");
  });
});

describe("sanitizeSegment", () => {
  it("trims and returns a valid name", () => {
    expect(sanitizeSegment("  note.md  ")).toBe("note.md");
  });
  it("rejects empty / separator / traversal names", () => {
    expect(sanitizeSegment("")).toBeNull();
    expect(sanitizeSegment("   ")).toBeNull();
    expect(sanitizeSegment("a/b.md")).toBeNull();
    expect(sanitizeSegment("a\\b.md")).toBeNull();
    expect(sanitizeSegment(".")).toBeNull();
    expect(sanitizeSegment("..")).toBeNull();
  });
});

describe("computeChildTarget", () => {
  it("joins dir + name, tolerating a trailing slash", () => {
    expect(computeChildTarget("/team/content/impl", "new.md")).toBe("/team/content/impl/new.md");
    expect(computeChildTarget("/team/content/impl/", "new.md")).toBe("/team/content/impl/new.md");
  });
  it("returns null for an invalid name", () => {
    expect(computeChildTarget("/x", "sub/b.md")).toBeNull();
    expect(computeChildTarget("/x", "")).toBeNull();
  });
});

describe("computeRenameTarget", () => {
  it("renames a sibling, preserving the directory", () => {
    expect(computeRenameTarget("/team/content/impl/old.md", "new.md")).toBe(
      "/team/content/impl/new.md",
    );
  });
  it("trims whitespace", () => {
    expect(computeRenameTarget("/x/a.md", "  b.md  ")).toBe("/x/b.md");
  });
  it("renames a directory (trailing slash tolerated)", () => {
    expect(computeRenameTarget("/x/epic-old/", "epic-new")).toBe("/x/epic-new");
  });
  it("rejects names with separators or traversal", () => {
    expect(computeRenameTarget("/x/a.md", "sub/b.md")).toBeNull();
    expect(computeRenameTarget("/x/a.md", "..")).toBeNull();
    expect(computeRenameTarget("/x/a.md", ".")).toBeNull();
    expect(computeRenameTarget("/x/a.md", "")).toBeNull();
    expect(computeRenameTarget("/x/a.md", "   ")).toBeNull();
  });
});

describe("deleteContent", () => {
  it("calls the command and reports success", async () => {
    const deleteContentFile = vi.fn().mockResolvedValue(undefined);
    expect(await deleteContent("/x/a.md", { deleteContentFile })).toEqual({ ok: true });
    expect(deleteContentFile).toHaveBeenCalledWith("/x/a.md");
  });
  it("surfaces a command error", async () => {
    const deleteContentFile = vi.fn().mockRejectedValue(new Error("boom"));
    expect(await deleteContent("/x/a.md", { deleteContentFile })).toEqual({
      ok: false,
      error: "boom",
    });
  });
});

describe("renameContent", () => {
  it("moves to the resolved sibling path", async () => {
    const moveContentFile = vi.fn().mockResolvedValue(undefined);
    const r = await renameContent("/x/old.md", "new.md", { moveContentFile });
    expect(r).toEqual({ ok: true, newPath: "/x/new.md" });
    expect(moveContentFile).toHaveBeenCalledWith("/x/old.md", "/x/new.md");
  });
  it("is a no-op when the name is unchanged", async () => {
    const moveContentFile = vi.fn();
    const r = await renameContent("/x/a.md", "a.md", { moveContentFile });
    expect(r.ok).toBe(true);
    expect(moveContentFile).not.toHaveBeenCalled();
  });
  it("rejects an invalid name before issuing a command", async () => {
    const moveContentFile = vi.fn();
    const r = await renameContent("/x/a.md", "bad/name.md", { moveContentFile });
    expect(r.ok).toBe(false);
    expect(moveContentFile).not.toHaveBeenCalled();
  });
});

describe("createContent", () => {
  it("creates an empty file at dir/name and reports the path", async () => {
    const createFile = vi.fn().mockResolvedValue(undefined);
    const r = await createContent("/team/content/impl", "new.md", { createFile });
    expect(r).toEqual({ ok: true, path: "/team/content/impl/new.md" });
    expect(createFile).toHaveBeenCalledWith("/team/content/impl/new.md", "");
  });
  it("rejects an invalid name before issuing the command", async () => {
    const createFile = vi.fn();
    const r = await createContent("/x", "sub/b.md", { createFile });
    expect(r.ok).toBe(false);
    expect(createFile).not.toHaveBeenCalled();
  });
  it("surfaces a collision (backend errors on existing file) so the caller can offer Replace", async () => {
    const createFile = vi.fn().mockRejectedValue(new Error("File already exists"));
    const r = await createContent("/x", "dupe.md", { createFile });
    expect(r).toEqual({
      ok: false,
      error: "File already exists",
      path: "/x/dupe.md",
      collision: true,
    });
  });
  it("flags a non-collision error as collision:false (not a Replace prompt)", async () => {
    const createFile = vi.fn().mockRejectedValue(new Error("permission denied"));
    const r = await createContent("/x", "f.md", { createFile });
    expect(r).toMatchObject({ ok: false, collision: false });
  });
});

describe("suffixName", () => {
  it("inserts the disambiguator before the extension", () => {
    expect(suffixName("report.md", 2)).toBe("report-2.md");
    expect(suffixName("a.b.md", 3)).toBe("a.b-3.md");
  });
  it("appends for a name without an extension or a dotfile", () => {
    expect(suffixName("notes", 2)).toBe("notes-2");
    expect(suffixName(".env", 2)).toBe(".env-2");
  });
});

describe("createContentUnique (Keep both — non-destructive)", () => {
  it("creates the exact name when free", async () => {
    const createFile = vi.fn().mockResolvedValue(undefined);
    const r = await createContentUnique("/x", "new.md", { createFile });
    expect(r).toEqual({ ok: true, path: "/x/new.md" });
    expect(createFile).toHaveBeenCalledTimes(1);
  });
  it("falls back to a numbered copy on collision, never overwriting", async () => {
    // first two names collide, the third is free
    const createFile = vi
      .fn()
      .mockRejectedValueOnce(new Error("File already exists"))
      .mockRejectedValueOnce(new Error("File already exists"))
      .mockResolvedValueOnce(undefined);
    const r = await createContentUnique("/x", "dupe.md", { createFile });
    expect(r).toEqual({ ok: true, path: "/x/dupe-3.md" });
    expect(createFile).toHaveBeenNthCalledWith(1, "/x/dupe.md", "");
    expect(createFile).toHaveBeenNthCalledWith(2, "/x/dupe-2.md", "");
    expect(createFile).toHaveBeenNthCalledWith(3, "/x/dupe-3.md", "");
  });
  it("stops on a non-collision error (does not spin)", async () => {
    const createFile = vi.fn().mockRejectedValue(new Error("permission denied"));
    const r = await createContentUnique("/x", "a.md", { createFile });
    expect(r).toMatchObject({ ok: false, error: "permission denied" });
    expect(createFile).toHaveBeenCalledTimes(1);
  });
});

describe("moveContentTo", () => {
  it("moves the file (keeping its basename) into another directory / scope", async () => {
    const moveContentFile = vi.fn().mockResolvedValue(undefined);
    const r = await moveContentTo("/team-repo/impl/a.md", "/team-workspace/shared", {
      moveContentFile,
    });
    expect(r).toEqual({ ok: true, newPath: "/team-workspace/shared/a.md" });
    expect(moveContentFile).toHaveBeenCalledWith(
      "/team-repo/impl/a.md",
      "/team-workspace/shared/a.md",
    );
  });
  it("is a no-op success when the destination dir is the current dir", async () => {
    const moveContentFile = vi.fn();
    const r = await moveContentTo("/x/impl/a.md", "/x/impl", { moveContentFile });
    expect(r.ok).toBe(true);
    expect(moveContentFile).not.toHaveBeenCalled();
  });
  it("surfaces a destination collision", async () => {
    const moveContentFile = vi.fn().mockRejectedValue(new Error("target already exists"));
    const r = await moveContentTo("/x/a.md", "/y", { moveContentFile });
    expect(r).toEqual({
      ok: false,
      error: "target already exists",
      newPath: "/y/a.md",
      collision: true,
    });
  });
});

describe("ingestFile (SW-VE-7 drag-drop)", () => {
  it("ingests the dropped source into dir/name via the gated command", async () => {
    const ingestFileIntoContext = vi.fn().mockResolvedValue(undefined);
    const r = await ingestFile("/Users/me/Downloads/pic.png", "/team/content/img", "pic.png", {
      ingestFileIntoContext,
    });
    expect(r).toEqual({ ok: true, path: "/team/content/img/pic.png" });
    expect(ingestFileIntoContext).toHaveBeenCalledWith(
      "/Users/me/Downloads/pic.png",
      "/team/content/img/pic.png",
    );
  });

  it("rejects an invalid name before issuing the command", async () => {
    const ingestFileIntoContext = vi.fn();
    const r = await ingestFile("/src/a", "/team/content", "..", { ingestFileIntoContext });
    expect(r).toMatchObject({ ok: false, error: "Invalid name" });
    expect(ingestFileIntoContext).not.toHaveBeenCalled();
  });

  it("surfaces a name collision non-destructively (collision flag set)", async () => {
    const ingestFileIntoContext = vi.fn().mockRejectedValue(new Error("File already exists: x"));
    const r = await ingestFile("/src/a.md", "/team/content", "a.md", { ingestFileIntoContext });
    expect(r).toMatchObject({ ok: false, path: "/team/content/a.md", collision: true });
  });
});

describe("ingestFileUnique (Keep both — non-destructive)", () => {
  it("ingests the exact name when free", async () => {
    const ingestFileIntoContext = vi.fn().mockResolvedValue(undefined);
    const r = await ingestFileUnique("/src/new.md", "/x", "new.md", { ingestFileIntoContext });
    expect(r).toEqual({ ok: true, path: "/x/new.md" });
    expect(ingestFileIntoContext).toHaveBeenCalledTimes(1);
  });

  it("falls back to a numbered copy on collision, never overwriting", async () => {
    const ingestFileIntoContext = vi
      .fn()
      .mockRejectedValueOnce(new Error("File already exists"))
      .mockResolvedValueOnce(undefined);
    const r = await ingestFileUnique("/src/dupe.md", "/x", "dupe.md", { ingestFileIntoContext });
    expect(r).toEqual({ ok: true, path: "/x/dupe-2.md" });
    expect(ingestFileIntoContext).toHaveBeenNthCalledWith(1, "/src/dupe.md", "/x/dupe.md");
    expect(ingestFileIntoContext).toHaveBeenNthCalledWith(2, "/src/dupe.md", "/x/dupe-2.md");
  });

  it("stops on a non-collision error (does not spin)", async () => {
    const ingestFileIntoContext = vi.fn().mockRejectedValue(new Error("too large"));
    const r = await ingestFileUnique("/src/a.md", "/x", "a.md", { ingestFileIntoContext });
    expect(r).toMatchObject({ ok: false, error: "too large" });
    expect(ingestFileIntoContext).toHaveBeenCalledTimes(1);
  });
});
