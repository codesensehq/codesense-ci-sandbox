// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  parseCsHref,
  formatCsHref,
  isCsHref,
  extractNoteLinks,
  resolveCsLink,
  buildCsLinkToDoc,
  toContentRelative,
  type CsLink,
} from "./note-links";

const CONTENT = "/home/u/org/codesense-context/_team/default/content";

describe("note-links — parse/format (SW-SN-9 Task 4)", () => {
  it("round-trips each kind through format→parse", () => {
    const links: CsLink[] = [
      { kind: "note", target: "18f2a-3" },
      { kind: "story", target: "SW-SN-2" },
      { kind: "file", target: "_bmad-output/x.md" },
    ];
    for (const link of links) {
      expect(parseCsHref(formatCsHref(link))).toEqual(link);
    }
  });

  it("keeps slashes and a #fragment in a file target (splits only the kind colon)", () => {
    const href = "cs:file:_bmad-output/a/b.md#heading";
    expect(parseCsHref(href)).toEqual({ kind: "file", target: "_bmad-output/a/b.md#heading" });
  });

  it("percent-encodes spaces/parens/# in the href and decodes them on parse (markdown-safe)", () => {
    const link: CsLink = { kind: "file", target: "ux/My Doc (v2)#draft.md" };
    const href = formatCsHref(link);
    expect(href).toBe("cs:file:ux/My%20Doc%20%28v2%29%23draft.md");
    expect(href).not.toMatch(/[ ()#]/); // safe inside `[label](href)`, no phantom fragment
    expect(parseCsHref(href)).toEqual(link);
  });

  it("round-trips a literal % in a target without double-encoding", () => {
    const link: CsLink = { kind: "file", target: "reports/50%-done.md" };
    expect(formatCsHref(link)).toBe("cs:file:reports/50%25-done.md");
    expect(parseCsHref(formatCsHref(link))).toEqual(link);
  });

  it("preserves multi-segment storyIds unchanged (R5 — no display-label collapse)", () => {
    expect(parseCsHref("cs:story:SW-SN-2-foo")).toEqual({ kind: "story", target: "SW-SN-2-foo" });
  });

  it("rejects non-cs, unknown kind, and empty target", () => {
    expect(parseCsHref("https://x.com")).toBeNull();
    expect(parseCsHref("cs:bogus:1")).toBeNull();
    expect(parseCsHref("cs:note:")).toBeNull();
    expect(parseCsHref("cs:note")).toBeNull();
    expect(isCsHref("cs:story:SW-SN-2")).toBe(true);
    expect(isCsHref("mailto:a@b.c")).toBe(false);
  });
});

describe("extractNoteLinks (SW-SN-9 AC-6)", () => {
  it("finds all three kinds in a markdown body, in order", () => {
    const body = [
      "See [the note](cs:note:abc) and",
      "[the story](cs:story:SW-SN-2) plus",
      "[a file](cs:file:_bmad-output/x.md).",
      "Ignore [external](https://x.com) and [rel](./y.md).",
    ].join("\n");
    expect(extractNoteLinks(body)).toEqual([
      { kind: "note", target: "abc" },
      { kind: "story", target: "SW-SN-2" },
      { kind: "file", target: "_bmad-output/x.md" },
    ]);
  });

  it("returns [] for a body with no cs: links", () => {
    expect(extractNoteLinks("just prose and [a link](https://x.com)")).toEqual([]);
  });

  it("captures an encoded space-containing file link and decodes it (SN-10 seam)", () => {
    expect(extractNoteLinks("[x](cs:file:ux/My%20Doc.md)")).toEqual([
      { kind: "file", target: "ux/My Doc.md" },
    ]);
  });
});

describe("resolveCsLink (SW-SN-9 AC-2/AC-3)", () => {
  const nodes = [
    { storyId: "SW-SN-2", storyFile: `${CONTENT}/_bmad-output/SW-SN-2.md` },
    { storyId: "SW-SN-2-foo", storyFile: `${CONTENT}/_bmad-output/SW-SN-2-foo.md` },
    { storyId: undefined, storyFile: undefined },
  ];

  it("note → noteId (existence checked by caller)", () => {
    expect(
      resolveCsLink({ kind: "note", target: "abc" }, { nodes, teamContentPath: CONTENT }),
    ).toEqual({ noteId: "abc" });
  });

  it("story → the node's storyFile, matched on the canonical id (incl. hyphens)", () => {
    expect(
      resolveCsLink({ kind: "story", target: "SW-SN-2-foo" }, { nodes, teamContentPath: CONTENT }),
    ).toEqual({ filePath: `${CONTENT}/_bmad-output/SW-SN-2-foo.md` });
  });

  it("story → null when the id is absent from the tree (AC-5 inert)", () => {
    expect(
      resolveCsLink({ kind: "story", target: "GONE-9" }, { nodes, teamContentPath: CONTENT }),
    ).toBeNull();
  });

  it("file → absolute path anchored at teamContentPath, hash forwarded", () => {
    expect(
      resolveCsLink(
        { kind: "file", target: "_bmad-output/a/b.md#sec" },
        { nodes, teamContentPath: CONTENT },
      ),
    ).toEqual({ filePath: `${CONTENT}/_bmad-output/a/b.md`, hash: "sec" });
  });

  it("file → null for a non-viewer extension (AC-5 inert)", () => {
    expect(
      resolveCsLink(
        { kind: "file", target: "_bmad-output/x.png" },
        { nodes, teamContentPath: CONTENT },
      ),
    ).toBeNull();
  });
});

describe("buildCsLinkToDoc — inverse of resolveCsLink (SW-SN-9 AC-6)", () => {
  it("prefers a stable storyId", () => {
    expect(buildCsLinkToDoc({ storyId: "SW-SN-2", filePath: `${CONTENT}/x.md` })).toEqual({
      kind: "story",
      target: "SW-SN-2",
    });
  });

  it("falls back to a content-root-relative file link", () => {
    expect(
      buildCsLinkToDoc({ filePath: `${CONTENT}/_bmad-output/x.md`, teamContentPath: CONTENT }),
    ).toEqual({ kind: "file", target: "_bmad-output/x.md" });
  });

  it("a story link built from an id resolves back to that story's file", () => {
    const nodes = [{ storyId: "SW-SN-2", storyFile: `${CONTENT}/_bmad-output/SW-SN-2.md` }];
    const link = buildCsLinkToDoc({ storyId: "SW-SN-2" })!;
    expect(resolveCsLink(link, { nodes, teamContentPath: CONTENT })).toEqual({
      filePath: `${CONTENT}/_bmad-output/SW-SN-2.md`,
    });
  });

  it("returns null when neither storyId nor filePath is given", () => {
    expect(buildCsLinkToDoc({})).toBeNull();
  });

  it("returns null for a file doc with no teamContentPath (won't leak a host path)", () => {
    expect(buildCsLinkToDoc({ filePath: "/abs/host/path/x.md" })).toBeNull();
  });
});

describe("toContentRelative", () => {
  it("strips the content root and leading slash", () => {
    expect(toContentRelative(`${CONTENT}/_bmad-output/x.md`, CONTENT)).toBe("_bmad-output/x.md");
  });
  it("leaves a path outside the root as-is (sans leading slash)", () => {
    expect(toContentRelative("/other/x.md", CONTENT)).toBe("other/x.md");
  });
});
