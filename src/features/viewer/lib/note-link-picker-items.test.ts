// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  buildPickerItems,
  filterPickerItems,
  pickerItemHref,
  PICKER_LIMIT,
  type DocNodeLike,
  type PickerNote,
} from "./note-link-picker-items";

const CONTENT = "/root/content";

const notes: PickerNote[] = [
  { id: "n1", title: "Prompts" },
  { id: "n2", title: "" },
];

const nodes: DocNodeLike[] = [
  {
    title: "Linkable notes",
    storyId: "SW-SN-9",
    storyFile: `${CONTENT}/_bmad-output/SW-SN-9.md`,
    resolvedSupportingDocs: [
      { fileName: "EXPERIENCE.md", filePath: `${CONTENT}/ux/EXPERIENCE.md` },
    ],
  },
  {
    // Group/epic node: title only, no storyId → contributes no story item.
    title: "Scratch Notes",
    resolvedSupportingDocs: [
      { fileName: "EXPERIENCE.md", filePath: `${CONTENT}/ux/EXPERIENCE.md` },
    ],
  },
];

describe("buildPickerItems (SW-SN-9 Task 2 / AC-1)", () => {
  const items = buildPickerItems(notes, nodes, CONTENT);

  it("emits notes (fallback label for untitled), stories, and files in order", () => {
    expect(items.map((i) => i.kind)).toEqual(["note", "note", "story", "file"]);
    expect(items[1].label).toBe("Untitled note");
    expect(items[2]).toMatchObject({ kind: "story", label: "Linkable notes", sublabel: "SW-SN-9" });
  });

  it("links stories by storyId and files by content-relative path", () => {
    expect(pickerItemHref(items[2])).toBe("cs:story:SW-SN-9");
    expect(pickerItemHref(items[3])).toBe("cs:file:ux/EXPERIENCE.md");
  });

  it("de-dupes a supporting doc shared across nodes (appears once)", () => {
    expect(items.filter((i) => i.kind === "file")).toHaveLength(1);
  });

  it("excludes group nodes without a storyId from story items", () => {
    expect(items.filter((i) => i.kind === "story")).toHaveLength(1);
  });

  it("excludes non-viewer file extensions the resolver would reject (e.g. images)", () => {
    const withImg: DocNodeLike[] = [
      {
        resolvedSupportingDocs: [{ fileName: "diagram.png", filePath: `${CONTENT}/x/diagram.png` }],
      },
    ];
    expect(buildPickerItems([], withImg, CONTENT).filter((i) => i.kind === "file")).toHaveLength(0);
  });

  it("omits file items until teamContentPath is known (no absolute-path leak)", () => {
    expect(buildPickerItems([], nodes, "").filter((i) => i.kind === "file")).toHaveLength(0);
  });

  it("excludes a supporting doc resolved outside the content root (no bogus link)", () => {
    const outside: DocNodeLike[] = [
      { resolvedSupportingDocs: [{ fileName: "shared.md", filePath: "/other/root/shared.md" }] },
    ];
    expect(buildPickerItems([], outside, CONTENT).filter((i) => i.kind === "file")).toHaveLength(0);
  });

  it("excludes a doc with a `#` in its path (ambiguous with the fragment delimiter)", () => {
    const hashed: DocNodeLike[] = [
      { resolvedSupportingDocs: [{ fileName: "my#doc.md", filePath: `${CONTENT}/x/my#doc.md` }] },
    ];
    expect(buildPickerItems([], hashed, CONTENT).filter((i) => i.kind === "file")).toHaveLength(0);
  });

  it("de-dupes story items sharing a storyId", () => {
    expect(
      buildPickerItems([], [nodes[0], { ...nodes[0] }], CONTENT).filter((i) => i.kind === "story"),
    ).toHaveLength(1);
  });

  it("excludes the active note (no self-link)", () => {
    const built = buildPickerItems(notes, [], CONTENT, "n1");
    expect(built.some((i) => i.link.target === "n1")).toBe(false);
    expect(built).toHaveLength(1); // only n2 remains
  });
});

describe("filterPickerItems", () => {
  const items = buildPickerItems(notes, nodes, CONTENT);

  it("matches label and sublabel, case-insensitively", () => {
    expect(filterPickerItems(items, "prompt").map((i) => i.label)).toEqual(["Prompts"]);
    expect(filterPickerItems(items, "sw-sn-9").map((i) => i.kind)).toEqual(["story"]);
    expect(filterPickerItems(items, "experience").map((i) => i.kind)).toEqual(["file"]);
  });

  it("returns everything (capped) for a blank query", () => {
    expect(filterPickerItems(items, "  ")).toHaveLength(items.length);
  });

  it("caps results at PICKER_LIMIT", () => {
    const many: PickerNote[] = Array.from({ length: PICKER_LIMIT + 10 }, (_, i) => ({
      id: `id${i}`,
      title: `note ${i}`,
    }));
    expect(filterPickerItems(buildPickerItems(many, [], CONTENT), "note")).toHaveLength(
      PICKER_LIMIT,
    );
  });
});
