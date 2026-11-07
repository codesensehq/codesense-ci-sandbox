// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { Node } from "@xyflow/react";

import { shouldShowRelatedChip, computeDocRelations, noteBodyLinksDoc } from "./doc-relations";
import { formatCsHref } from "./note-links";

const CONTENT = "/ctx/content";
const STORY = "/ctx/content/epic-a/SW-1.md";
const STORY_DOC = "/ctx/content/epic-a/diagram.excalidraw";
const EPIC_DOC = "/ctx/content/epic-a/README.md";
const EPIC_SUPPORT = "/ctx/content/epic-a/architecture.md";

const nodes = [
  {
    id: "s-1",
    type: "story",
    position: { x: 0, y: 0 },
    data: {
      title: "SW-1",
      documentType: "story",
      storyId: "SW-1",
      storyFile: STORY,
      resolvedSupportingDocs: [
        { fileName: "diagram.excalidraw", filePath: STORY_DOC, documentType: "canvas" },
      ],
    },
  },
  {
    id: "g-epic-a",
    type: "group",
    position: { x: 0, y: 0 },
    data: {
      title: "Epic A",
      storyId: "EPIC-A",
      mainDocFilePath: EPIC_DOC,
      resolvedSupportingDocs: [
        { fileName: "architecture.md", filePath: EPIC_SUPPORT, documentType: "doc" },
      ],
    },
  },
] as unknown as Node[];

describe("shouldShowRelatedChip (SW-SN-10 AC-1)", () => {
  it("shows for markdown and HTML docs", () => {
    expect(shouldShowRelatedChip("/x/a.md", false)).toBe(true);
    expect(shouldShowRelatedChip("/x/a.html", false)).toBe(true);
  });
  it("hides for non-doc viewers, empty tab, and sync-diff view", () => {
    expect(shouldShowRelatedChip("/x/a.excalidraw", false)).toBe(false);
    expect(shouldShowRelatedChip("/x/a.png", false)).toBe(false);
    expect(shouldShowRelatedChip("", false)).toBe(false);
    expect(shouldShowRelatedChip("/x/a.md", true)).toBe(false);
  });
});

describe("computeDocRelations (SW-SN-10 Task 2 / Decision #2)", () => {
  it("returns a story's own supporting docs + storyId identity", () => {
    const r = computeDocRelations(nodes, STORY, CONTENT);
    expect(r.supportingDocs).toHaveLength(1);
    expect(r.supportingDocs[0].filePath).toBe(STORY_DOC);
    expect(r.docIdentity).toEqual({ storyId: "SW-1" });
  });

  it("keys an epic (group) doc by PATH, not storyId (SN-9 can't resolve group ids)", () => {
    const r = computeDocRelations(nodes, EPIC_DOC, CONTENT);
    expect(r.supportingDocs[0].filePath).toBe(EPIC_SUPPORT);
    expect(r.docIdentity).toEqual({ filePath: EPIC_DOC, teamContentPath: CONTENT });
  });

  it("own-doc-only: an open supporting doc surfaces NO owner siblings", () => {
    const r = computeDocRelations(nodes, STORY_DOC, CONTENT);
    expect(r.supportingDocs).toHaveLength(0);
    expect(r.docIdentity).toEqual({ filePath: STORY_DOC, teamContentPath: CONTENT });
  });
});

describe("noteBodyLinksDoc (SW-SN-10 Task 3)", () => {
  const ctx = {
    nodes: nodes.map((n) => n.data as { storyId?: string; storyFile?: string }),
    teamContentPath: CONTENT,
  };

  it("matches a note whose cs:story link resolves to the open doc", () => {
    const body = `see [SW-1](${formatCsHref({ kind: "story", target: "SW-1" })})`;
    expect(noteBodyLinksDoc(body, STORY, ctx)).toBe(true);
  });

  it("matches a note whose cs:file link resolves to the open doc", () => {
    const body = `[epic](${formatCsHref({ kind: "file", target: "epic-a/README.md" })})`;
    expect(noteBodyLinksDoc(body, EPIC_DOC, ctx)).toBe(true);
  });

  it("does NOT match a note linking a different doc", () => {
    const body = `[other](${formatCsHref({ kind: "story", target: "SW-1" })})`;
    expect(noteBodyLinksDoc(body, EPIC_DOC, ctx)).toBe(false);
  });

  it("does NOT match a note with no cs: links, or an empty active path", () => {
    expect(noteBodyLinksDoc("plain text", STORY, ctx)).toBe(false);
    expect(
      noteBodyLinksDoc(`[x](${formatCsHref({ kind: "story", target: "SW-1" })})`, "", ctx),
    ).toBe(false);
  });
});
