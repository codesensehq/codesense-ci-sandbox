// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { createMockState } from "../mock-state";
import { contentFileHandlers } from "./content-files";
import type { FileEntry } from "@/tauri/commands";

describe("contentFileHandlers", () => {
  it("writes then reads a file back via list_directory", () => {
    const h = contentFileHandlers(createMockState());
    h.write_content_file({ path: "notes/todo.md", content: "hello" });
    const entries = h.list_directory({ dirPath: "notes" }) as FileEntry[];
    expect(entries).toEqual([
      { name: "todo.md", path: "notes/todo.md", is_dir: false, extension: "md", size: 5 },
    ]);
  });

  it("write overwrites content in place (edit)", () => {
    const state = createMockState();
    const h = contentFileHandlers(state);
    h.write_content_file({ path: "a.md", content: "one" });
    h.write_content_file({ path: "a.md", content: "two" });
    const entries = h.list_directory({ dirPath: "" }) as FileEntry[];
    expect(entries).toHaveLength(1);
    expect(entries[0].size).toBe(3);
  });

  it("list_directory surfaces immediate subdirectories, not nested files", () => {
    const h = contentFileHandlers(createMockState());
    h.write_content_file({ path: "docs/guide/intro.md", content: "x" });
    h.write_content_file({ path: "docs/readme.md", content: "y" });
    const entries = (h.list_directory({ dirPath: "docs" }) as FileEntry[]).sort((a, b) =>
      a.name.localeCompare(b.name),
    );
    expect(entries).toEqual([
      { name: "guide", path: "docs/guide", is_dir: true, extension: null, size: null },
      { name: "readme.md", path: "docs/readme.md", is_dir: false, extension: "md", size: 1 },
    ]);
  });

  it("moves a file to a new path", () => {
    const state = createMockState();
    const h = contentFileHandlers(state);
    h.write_content_file({ path: "old.md", content: "keep" });
    h.move_content_file({ fromPath: "old.md", toPath: "new.md" });
    const entries = h.list_directory({ dirPath: "" }) as FileEntry[];
    expect(entries.map((e) => e.name)).toEqual(["new.md"]);
    expect(entries[0].size).toBe(4);
  });

  it("deletes a file", () => {
    const state = createMockState();
    const h = contentFileHandlers(state);
    h.write_content_file({ path: "gone.md", content: "x" });
    h.delete_content_file({ path: "gone.md" });
    expect(h.list_directory({ dirPath: "" })).toEqual([]);
  });

  it("throws (not a silent no-op) when moving a file that does not exist", () => {
    const h = contentFileHandlers(createMockState());
    expect(() => h.move_content_file({ fromPath: "ghost.md", toPath: "x.md" })).toThrow();
  });

  it("refuses to silently overwrite an occupied destination on move", () => {
    const state = createMockState();
    const h = contentFileHandlers(state);
    h.write_content_file({ path: "a.md", content: "aaa" });
    h.write_content_file({ path: "b.md", content: "bbb" });
    expect(() => h.move_content_file({ fromPath: "a.md", toPath: "b.md" })).toThrow();
    // Both files survive the refused move.
    expect(
      (h.list_directory({ dirPath: "" }) as Array<{ name: string }>).map((e) => e.name).sort(),
    ).toEqual(["a.md", "b.md"]);
  });

  it("get_story_details reads the frontmatter title, H1, and HTML cs:title (SW-LN-34)", () => {
    const h = contentFileHandlers(createMockState());
    h.write_content_file({
      path: "epic/SW-LN-23-proof.md",
      content: '---\ntitle: "Proof by default: evidence"\nstory_id: SW-LN-23\n---\n# Body H1\n',
    });
    expect(h.get_story_details({ filePath: "epic/SW-LN-23-proof.md" })).toEqual({
      title: "Body H1",
      storyText: null,
      titleMeta: "Proof by default: evidence",
    });
    h.write_content_file({
      path: "epic/notes.html",
      content: '<meta name="cs:title" content="Html title"><title>Doc</title>',
    });
    expect(h.get_story_details({ filePath: "epic/notes.html" })).toEqual({
      title: "Doc",
      storyText: null,
      titleMeta: "Html title",
    });
    h.write_content_file({ path: "epic/plain.md", content: "no heading" });
    expect(h.get_story_details({ filePath: "epic/plain.md" })).toEqual({
      title: null,
      storyText: null,
      titleMeta: null,
    });
  });
});
