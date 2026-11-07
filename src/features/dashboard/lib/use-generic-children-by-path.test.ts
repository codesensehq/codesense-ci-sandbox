// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import type { FileEntry } from "@/tauri/commands";

const mockListDirectory = vi.fn<(path: string) => Promise<FileEntry[]>>();
vi.mock("@/tauri/commands", () => ({
  listDirectory: (path: string) => mockListDirectory(path),
}));

const { useGenericChildrenByPath, pipelineTransform } =
  await import("./use-generic-children-by-path");

function makeEntry(name: string, parent: string, opts: Partial<FileEntry> = {}): FileEntry {
  return {
    name,
    path: `${parent}/${name}`,
    is_dir: false,
    extension: name.split(".").pop() ?? null,
    size: null,
    ...opts,
  };
}

beforeEach(() => {
  mockListDirectory.mockReset();
});

describe("useGenericChildrenByPath", () => {
  it("returns empty map when no paths expanded", async () => {
    const expanded = new Set<string>();
    const { result } = renderHook(() => useGenericChildrenByPath({ expandedPaths: expanded }));
    await waitFor(() => expect(result.current.size).toBe(0));
    expect(mockListDirectory).not.toHaveBeenCalled();
  });

  // SW-VE-1: listing is not rendering — every real child lists (incl. the
  // non-viewable `binary.bin`), minus only the noise exclusions.
  it("lists every real child incl. non-viewable files, excluding only noise", async () => {
    mockListDirectory.mockResolvedValueOnce([
      makeEntry("sub-dir", "/viewer/docs", { is_dir: true, extension: null }),
      makeEntry("guide.md", "/viewer/docs", { extension: "md" }),
      makeEntry("CLAUDE.md", "/viewer/docs", { extension: "md" }),
      makeEntry(".meta", "/viewer/docs", { is_dir: true, extension: null }),
      makeEntry("binary.bin", "/viewer/docs", { extension: "bin" }),
    ]);

    const expanded = new Set(["/viewer/docs"]);
    const { result } = renderHook(() => useGenericChildrenByPath({ expandedPaths: expanded }));

    await waitFor(() => expect(result.current.size).toBe(1));
    const children = result.current.get("/viewer/docs")!;
    // binary.bin now LISTS (VE-1); `.meta` lists like any dot dir; only CLAUDE.md
    // stays excluded. Natural-sorted.
    expect(children.map((c) => c.name)).toEqual([".meta", "binary.bin", "guide.md", "sub-dir"]);
  });

  it("prunes entries when a path leaves expandedPaths", async () => {
    mockListDirectory.mockImplementation(async (p) => [
      makeEntry("file.md", p, { extension: "md" }),
    ]);

    const { result, rerender } = renderHook(
      (props: { expanded: Set<string> }) =>
        useGenericChildrenByPath({ expandedPaths: props.expanded }),
      { initialProps: { expanded: new Set(["/viewer/A", "/viewer/B"]) } },
    );

    await waitFor(() => expect(result.current.size).toBe(2));

    rerender({ expanded: new Set(["/viewer/A"]) });
    await waitFor(() => expect(result.current.size).toBe(1));
    expect(result.current.has("/viewer/A")).toBe(true);
    expect(result.current.has("/viewer/B")).toBe(false);
  });

  it("re-fetches when refreshKey bumps", async () => {
    mockListDirectory.mockImplementation(async (p) => [
      makeEntry("file.md", p, { extension: "md" }),
    ]);

    const expanded = new Set(["/viewer/docs"]);
    const { rerender } = renderHook(
      (props: { refreshKey: number }) =>
        useGenericChildrenByPath({ expandedPaths: expanded, refreshKey: props.refreshKey }),
      { initialProps: { refreshKey: 0 } },
    );

    await waitFor(() => expect(mockListDirectory).toHaveBeenCalledTimes(1));
    rerender({ refreshKey: 1 });
    await waitFor(() => expect(mockListDirectory).toHaveBeenCalledTimes(2));
  });

  it("handles listDirectory rejection silently", async () => {
    mockListDirectory.mockRejectedValueOnce(new Error("boom"));

    const expanded = new Set(["/viewer/docs"]);
    const { result } = renderHook(() => useGenericChildrenByPath({ expandedPaths: expanded }));

    await waitFor(() => expect(result.current.size).toBe(1));
    expect(result.current.get("/viewer/docs")).toEqual([]);
  });

  // -- NB-7 / AC-6: delta-based fetching --

  it("only fetches newly-added paths (delta fetch, NB-7 AC-6)", async () => {
    mockListDirectory.mockImplementation(async (p) => [makeEntry("f.md", p, { extension: "md" })]);

    const { rerender } = renderHook(
      (props: { expanded: Set<string> }) =>
        useGenericChildrenByPath({ expandedPaths: props.expanded }),
      { initialProps: { expanded: new Set(["/viewer/A", "/viewer/B"]) } },
    );

    await waitFor(() => expect(mockListDirectory).toHaveBeenCalledTimes(2));

    // Add C only — exactly 1 additional fetch.
    rerender({ expanded: new Set(["/viewer/A", "/viewer/B", "/viewer/C"]) });
    await waitFor(() => expect(mockListDirectory).toHaveBeenCalledTimes(3));
  });

  it("collapse-only transition fires no fetches (NB-7 AC-6)", async () => {
    mockListDirectory.mockImplementation(async (p) => [makeEntry("f.md", p, { extension: "md" })]);

    const { result, rerender } = renderHook(
      (props: { expanded: Set<string> }) =>
        useGenericChildrenByPath({ expandedPaths: props.expanded }),
      { initialProps: { expanded: new Set(["/viewer/A", "/viewer/B"]) } },
    );

    await waitFor(() => expect(mockListDirectory).toHaveBeenCalledTimes(2));

    rerender({ expanded: new Set(["/viewer/A"]) });
    await waitFor(() => expect(result.current.has("/viewer/B")).toBe(false));
    expect(mockListDirectory).toHaveBeenCalledTimes(2);
  });

  it("re-expanding a collapsed path re-fetches (NB-7 AC-6)", async () => {
    mockListDirectory.mockImplementation(async (p) => [makeEntry("f.md", p, { extension: "md" })]);

    const { rerender } = renderHook(
      (props: { expanded: Set<string> }) =>
        useGenericChildrenByPath({ expandedPaths: props.expanded }),
      { initialProps: { expanded: new Set(["/viewer/A"]) } },
    );

    await waitFor(() => expect(mockListDirectory).toHaveBeenCalledTimes(1));

    rerender({ expanded: new Set() });
    expect(mockListDirectory).toHaveBeenCalledTimes(1);

    rerender({ expanded: new Set(["/viewer/A"]) });
    await waitFor(() => expect(mockListDirectory).toHaveBeenCalledTimes(2));
  });
});

// -- SW-VE-1 AC-1: the transform itself (the empty-folder-bug fix) ------------
describe("pipelineTransform (SW-VE-1)", () => {
  const names = (entries: FileEntry[]) => entries.map((e) => e.name);

  it("lists a config-only folder in full — the `bmad/_config` empty-expand bug is gone", () => {
    const config = [
      makeEntry("core.yaml", "/d", { extension: "yaml" }),
      makeEntry("agents.toml", "/d", { extension: "toml" }),
      makeEntry("manifest.json", "/d", { extension: "json" }),
      makeEntry("data.xml", "/d", { extension: "xml" }),
    ];
    // Before VE-1 this filtered to [] (none are viewer files) → dead expand.
    expect(names(pipelineTransform(config))).toEqual([
      "agents.toml",
      "core.yaml",
      "data.xml",
      "manifest.json",
    ]);
  });

  it("keeps directories and non-viewable files; natural-sorts", () => {
    const entries = [
      makeEntry("script.sh", "/d", { extension: "sh" }),
      makeEntry("nested", "/d", { is_dir: true, extension: null }),
      makeEntry("archive.zip", "/d", { extension: "zip" }),
      makeEntry("notes.md", "/d", { extension: "md" }),
    ];
    expect(names(pipelineTransform(entries))).toEqual([
      "archive.zip",
      "nested",
      "notes.md",
      "script.sh",
    ]);
  });

  it("still excludes ONLY the intentional noise (CLAUDE.md, derived renders)", () => {
    const entries = [
      makeEntry("keep.yaml", "/d", { extension: "yaml" }),
      makeEntry("CLAUDE.md", "/d", { extension: "md" }),
      makeEntry(".meta", "/d", { is_dir: true, extension: null }),
      makeEntry("design.excalidraw.png", "/d", { extension: "png" }),
      makeEntry(".gitignore", "/d", { extension: null }),
    ];
    // Dotfiles like `.gitignore` and the `.meta` dir are real content and DO
    // list; only CLAUDE.md and derived renders are dropped.
    expect(names(pipelineTransform(entries))).toEqual([".gitignore", ".meta", "keep.yaml"]);
  });
});
