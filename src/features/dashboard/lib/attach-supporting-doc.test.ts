// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import {
  attachSupportingDoc,
  detachSupportingDoc,
  relativeSupportingDocPath,
  removeSupportingDoc,
  upsertSupportingDoc,
  frontmatterType,
} from "./attach-supporting-doc";

describe("relativeSupportingDocPath", () => {
  it("returns ./name for a sibling canvas", () => {
    expect(relativeSupportingDocPath("/ctx/epic/story.md", "/ctx/epic/design.excalidraw")).toBe(
      "./design.excalidraw",
    );
  });
  it("descends into a subdirectory", () => {
    expect(relativeSupportingDocPath("/ctx/epic/story.md", "/ctx/epic/art/d.excalidraw")).toBe(
      "./art/d.excalidraw",
    );
  });
  it("climbs to a sibling directory", () => {
    expect(relativeSupportingDocPath("/ctx/epic/story.md", "/ctx/other/d.excalidraw")).toBe(
      "../other/d.excalidraw",
    );
  });
});

describe("upsertSupportingDoc", () => {
  it("adds a block list when supporting_docs is absent", () => {
    const story = `---\ntype: story\nstory_id: X\n---\n\n# Story\n`;
    const { text, changed } = upsertSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).toContain("supporting_docs:\n  - ./design.excalidraw");
    expect(text).toContain("# Story"); // body preserved
    expect(text).toContain("type: story"); // existing keys preserved
  });

  it("appends to an existing block list", () => {
    const story = `---\ntype: story\nsupporting_docs:\n  - ./notes.md\n---\nbody\n`;
    const { text, changed } = upsertSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).toContain("  - ./notes.md\n  - ./design.excalidraw");
  });

  it("is idempotent for a path already present (block list, ./-insensitive)", () => {
    const story = `---\nsupporting_docs:\n  - design.excalidraw\n---\n`;
    const { changed } = upsertSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(false);
  });

  it("appends to a flow list and stays idempotent", () => {
    const story = `---\nsupporting_docs: [./a.md]\n---\n`;
    const added = upsertSupportingDoc(story, "./design.excalidraw");
    expect(added.changed).toBe(true);
    expect(added.text).toContain("supporting_docs: [./a.md, ./design.excalidraw]");
    expect(upsertSupportingDoc(added.text, "./design.excalidraw").changed).toBe(false);
  });

  it("edits CRLF frontmatter in place without prepending a second block", () => {
    const story = `---\r\ntype: story\r\nsupporting_docs:\r\n  - ./notes.md\r\n---\r\n\r\nbody\r\n`;
    const { text, changed } = upsertSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    // Exactly one frontmatter block (no duplicate `---` prepended).
    expect(text.match(/^---$/gm)?.length).toBe(2);
    expect(text).toContain("  - ./notes.md\r\n  - ./design.excalidraw");
    expect(text).not.toContain("\n\n---"); // no LF-only block injected
  });

  it("handles a flow list with a trailing inline comment without corrupting YAML", () => {
    const story = `---\nsupporting_docs: [./a.md] # the docs\n---\n`;
    const { text, changed } = upsertSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).toContain("supporting_docs: [./a.md, ./design.excalidraw]");
    // Idempotent on the rewritten (comment-dropped) line.
    expect(upsertSupportingDoc(text, "./design.excalidraw").changed).toBe(false);
  });

  it("edits in place when a fence has trailing whitespace (matches extractFrontmatterBlock)", () => {
    const story = `--- \ntype: story\n--- \n\nbody\n`;
    const { text, changed } = upsertSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    // One frontmatter region, edited in place — not a second prepended block.
    expect(text.match(/^---\s*$/gm)?.length).toBe(2);
    expect(text).toContain("supporting_docs:\n  - ./design.excalidraw");
  });

  it("creates frontmatter when the file has none", () => {
    const story = `# Just a heading\n`;
    const { text, changed } = upsertSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text.startsWith("---\nsupporting_docs:\n  - ./design.excalidraw\n---\n")).toBe(true);
    expect(text).toContain("# Just a heading");
  });
});

describe("removeSupportingDoc", () => {
  // SLW-FX-21 AC-1: the missing unlink direction — mirrors upsertSupportingDoc's
  // normalize/EOL/flow-vs-block rigor.
  it("removes one item from a block list, keeping the others", () => {
    const story = `---\ntype: story\nsupporting_docs:\n  - ./notes.md\n  - ./design.excalidraw\n---\nbody\n`;
    const { text, changed } = removeSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).toContain("  - ./notes.md");
    expect(text).not.toContain("design.excalidraw");
    expect(text).toContain("body"); // body preserved
    expect(text).toContain("type: story"); // untouched keys preserved
  });

  it("is ./-insensitive when matching the ref to remove", () => {
    const story = `---\nsupporting_docs:\n  - design.excalidraw\n  - ./notes.md\n---\n`;
    const { text, changed } = removeSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).not.toContain("design.excalidraw");
    expect(text).toContain("  - ./notes.md");
  });

  it("removes one item from a flow list, keeping the others", () => {
    const story = `---\nsupporting_docs: [./a.md, ./design.excalidraw]\n---\n`;
    const { text, changed } = removeSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).toContain("supporting_docs: [./a.md]");
  });

  it("drops the whole supporting_docs key when the last block item is removed", () => {
    const story = `---\ntype: story\nsupporting_docs:\n  - ./design.excalidraw\n---\n\nbody\n`;
    const { text, changed } = removeSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).not.toContain("supporting_docs");
    expect(text).not.toContain("design.excalidraw");
    expect(text).toContain("type: story");
    expect(text).toContain("body");
    // Frontmatter fences intact (still exactly one block).
    expect(text.match(/^---$/gm)?.length).toBe(2);
  });

  it("drops the whole supporting_docs key when the last flow item is removed", () => {
    const story = `---\nsupporting_docs: [./design.excalidraw]\ntype: story\n---\n`;
    const { text, changed } = removeSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).not.toContain("supporting_docs");
    expect(text).toContain("type: story");
  });

  it("returns changed:false and writes nothing when the ref is not present", () => {
    const story = `---\nsupporting_docs:\n  - ./notes.md\n---\n`;
    const res = removeSupportingDoc(story, "./design.excalidraw");
    expect(res.changed).toBe(false);
    expect(res.text).toBe(story); // byte-for-byte unchanged
  });

  it("returns changed:false when there is no supporting_docs key at all", () => {
    const story = `---\ntype: story\n---\nbody\n`;
    const res = removeSupportingDoc(story, "./design.excalidraw");
    expect(res.changed).toBe(false);
    expect(res.text).toBe(story);
  });

  it("returns changed:false when the file has no frontmatter", () => {
    const story = `# Just a heading\n`;
    const res = removeSupportingDoc(story, "./design.excalidraw");
    expect(res.changed).toBe(false);
    expect(res.text).toBe(story);
  });

  it("preserves CRLF line endings when removing from a block list", () => {
    const story = `---\r\ntype: story\r\nsupporting_docs:\r\n  - ./notes.md\r\n  - ./design.excalidraw\r\n---\r\n\r\nbody\r\n`;
    const { text, changed } = removeSupportingDoc(story, "./design.excalidraw");
    expect(changed).toBe(true);
    expect(text).toContain("  - ./notes.md\r\n");
    expect(text).not.toContain("design.excalidraw");
    expect(text).not.toContain("\n\n"); // no lone-LF injected into a CRLF file
    expect(text.match(/^---\r?$/gm)?.length).toBe(2);
  });

  it("is idempotent — removing an already-absent ref is a no-op", () => {
    const story = `---\nsupporting_docs:\n  - ./a.md\n  - ./design.excalidraw\n---\n`;
    const first = removeSupportingDoc(story, "./design.excalidraw");
    expect(first.changed).toBe(true);
    const second = removeSupportingDoc(first.text, "./design.excalidraw");
    expect(second.changed).toBe(false);
    expect(second.text).toBe(first.text);
  });
});

describe("detachSupportingDoc", () => {
  const STORY_WITH = `---\ntype: story\nsupporting_docs:\n  - ./design.excalidraw\n  - ./notes.md\n---\nbody\n`;

  it("reads, removes, and writes the node .md, returning changed:true", async () => {
    const readFile = vi.fn().mockResolvedValue(STORY_WITH);
    const writeContentFile = vi.fn().mockResolvedValue(undefined);

    const res = await detachSupportingDoc("/ctx/epic/story.md", "/ctx/epic/design.excalidraw", {
      readFile,
      writeContentFile,
    });

    expect(res).toEqual({ changed: true });
    expect(readFile).toHaveBeenCalledWith("/ctx/epic/story.md");
    const [path, text] = writeContentFile.mock.calls[0];
    expect(path).toBe("/ctx/epic/story.md");
    expect(text).not.toContain("design.excalidraw");
    expect(text).toContain("  - ./notes.md");
  });

  it("performs NO write and returns changed:false when the doc isn't listed", async () => {
    const readFile = vi.fn().mockResolvedValue(STORY_WITH);
    const writeContentFile = vi.fn().mockResolvedValue(undefined);

    const res = await detachSupportingDoc("/ctx/epic/story.md", "/ctx/epic/absent.md", {
      readFile,
      writeContentFile,
    });

    expect(res).toEqual({ changed: false });
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("resolves the relative ref across directories (climbs with ../) before removing", async () => {
    const readFile = vi
      .fn()
      .mockResolvedValue(`---\nsupporting_docs:\n  - ../other/notes.md\n---\n`);
    const writeContentFile = vi.fn().mockResolvedValue(undefined);

    const res = await detachSupportingDoc("/ctx/epic/story.md", "/ctx/other/notes.md", {
      readFile,
      writeContentFile,
    });

    expect(res.changed).toBe(true);
    expect(writeContentFile.mock.calls[0][1]).not.toContain("notes.md");
  });

  it("propagates a read failure to the caller (no swallow)", async () => {
    const readFile = vi.fn().mockRejectedValue(new Error("disk gone"));
    const writeContentFile = vi.fn();

    await expect(
      detachSupportingDoc("/ctx/epic/story.md", "/ctx/epic/x.md", { readFile, writeContentFile }),
    ).rejects.toThrow("disk gone");
    expect(writeContentFile).not.toHaveBeenCalled();
  });
});

describe("attachSupportingDoc", () => {
  const STORY = `---\ntype: story\nstory_id: X\n---\n\n# Story\n`;

  it("reads, upserts, and writes the node .md, returning changed:true", async () => {
    const readFile = vi.fn().mockResolvedValue(STORY);
    const writeContentFile = vi.fn().mockResolvedValue(undefined);

    const res = await attachSupportingDoc("/ctx/epic/story.md", "/ctx/epic/design.excalidraw", {
      readFile,
      writeContentFile,
    });

    expect(res).toEqual({ changed: true });
    expect(readFile).toHaveBeenCalledWith("/ctx/epic/story.md");
    const [path, text] = writeContentFile.mock.calls[0];
    expect(path).toBe("/ctx/epic/story.md");
    // Relative ref computed from the node dir + upserted into the block list.
    expect(text).toContain("supporting_docs:\n  - ./design.excalidraw");
  });

  it("resolves the relative ref across directories (climbs with ../)", async () => {
    const readFile = vi.fn().mockResolvedValue(STORY);
    const writeContentFile = vi.fn().mockResolvedValue(undefined);

    await attachSupportingDoc("/ctx/epic/story.md", "/ctx/other/notes.md", {
      readFile,
      writeContentFile,
    });

    expect(writeContentFile.mock.calls[0][1]).toContain("  - ../other/notes.md");
  });

  it("is idempotent — an already-listed doc performs NO write and returns changed:false", async () => {
    const readFile = vi
      .fn()
      .mockResolvedValue(`---\ntype: story\nsupporting_docs:\n  - ./design.excalidraw\n---\n`);
    const writeContentFile = vi.fn().mockResolvedValue(undefined);

    const res = await attachSupportingDoc("/ctx/epic/story.md", "/ctx/epic/design.excalidraw", {
      readFile,
      writeContentFile,
    });

    expect(res).toEqual({ changed: false });
    expect(writeContentFile).not.toHaveBeenCalled();
  });

  it("creates frontmatter when the node .md has none", async () => {
    const readFile = vi.fn().mockResolvedValue(`# Just a heading\n`);
    const writeContentFile = vi.fn().mockResolvedValue(undefined);

    await attachSupportingDoc("/ctx/epic/README.md", "/ctx/epic/diagram.excalidraw", {
      readFile,
      writeContentFile,
    });

    expect(writeContentFile.mock.calls[0][1]).toContain(
      "---\nsupporting_docs:\n  - ./diagram.excalidraw\n---",
    );
  });

  it("propagates a read/write failure to the caller (no swallow)", async () => {
    const readFile = vi.fn().mockRejectedValue(new Error("disk gone"));
    const writeContentFile = vi.fn();

    await expect(
      attachSupportingDoc("/ctx/epic/story.md", "/ctx/epic/x.md", { readFile, writeContentFile }),
    ).rejects.toThrow("disk gone");
    expect(writeContentFile).not.toHaveBeenCalled();
  });
});

describe("frontmatterType", () => {
  it("reads the type field", () => {
    expect(frontmatterType(`---\ntype: story\n---\n`)).toBe("story");
    expect(frontmatterType(`---\ntype: "bug"\n---\n`)).toBe("bug");
  });
  it("strips an inline comment from an unquoted value", () => {
    expect(frontmatterType(`---\ntype: story # the canvas one\n---\n`)).toBe("story");
  });
  it("returns null when absent", () => {
    expect(frontmatterType(`---\nstory_id: X\n---\n`)).toBeNull();
    expect(frontmatterType(`no frontmatter`)).toBeNull();
  });
});
