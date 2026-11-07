// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkRehype from "remark-rehype";
import rehypeSlug from "rehype-slug";
import type { Element, Root as HastRoot, RootContent } from "hast";

import { buildRedlineHast, surfaceFrontmatter } from "./redline";

// Mirror the worker's slug step so the duplicate-heading id renumbering the
// signature must tolerate is actually exercised.
const proc = unified().use(remarkParse).use(remarkRehype).use(rehypeSlug);
function md(text: string): HastRoot {
  return proc.runSync(proc.parse(text)) as HastRoot;
}

function textOf(node: RootContent): string {
  if (node.type === "text") return node.value;
  if (node.type === "element") {
    return node.children.map((c) => textOf(c as RootContent)).join("");
  }
  return "";
}

/** Summarize the redline root: one entry per significant top-level child as
 *  `kind:text`, where kind is `ins`/`del` for wrapped blocks or the tag name. */
function summarize(root: HastRoot): string[] {
  return root.children
    .filter((c) => c.type === "element" || (c.type === "text" && c.value.trim() !== ""))
    .map((c) => {
      const el = c as Element;
      const tag = c.type === "element" ? el.tagName : c.type;
      if (tag === "ins" || tag === "del") {
        const inner = el.children[0] as Element;
        return `${tag}:${textOf(inner as RootContent).trim()}`;
      }
      return `${tag}:${textOf(c).trim()}`;
    });
}

describe("buildRedlineHast (SW-GS-3 redline core)", () => {
  it("added-whole-doc → every block wrapped <ins>", () => {
    const out = buildRedlineHast(md(""), md("# Title\n\nbody"));
    expect(summarize(out)).toEqual(["ins:Title", "ins:body"]);
  });

  it("deleted-whole-doc → every block wrapped <del>", () => {
    const out = buildRedlineHast(md("# Title\n\nbody"), md(""));
    expect(summarize(out)).toEqual(["del:Title", "del:body"]);
  });

  it("pure append → existing blocks untouched, new block <ins>", () => {
    const out = buildRedlineHast(md("# A\n\npara one"), md("# A\n\npara one\n\npara two"));
    expect(summarize(out)).toEqual(["h1:A", "p:para one", "ins:para two"]);
  });

  it("mid-doc edit → removed <del> + added <ins>, neighbors intact", () => {
    const out = buildRedlineHast(
      md("intro\n\nold line\n\noutro"),
      md("intro\n\nnew line\n\noutro"),
    );
    expect(summarize(out)).toEqual(["p:intro", "del:old line", "ins:new line", "p:outro"]);
  });

  it("identical docs → no marks", () => {
    const out = buildRedlineHast(md("# A\n\nsame"), md("# A\n\nsame"));
    expect(summarize(out)).toEqual(["h1:A", "p:same"]);
  });

  it("a heading and a paragraph with the same words do not align", () => {
    // Signature includes the tag, so changing a paragraph into a heading reads
    // as a remove + add, not an unchanged block.
    const out = buildRedlineHast(md("foo"), md("# foo"));
    expect(summarize(out)).toEqual(["del:foo", "ins:foo"]);
  });

  it("an inline-only change (link href) is NOT silently unchanged", () => {
    // Same text, different href: the structural signature catches it so the
    // block is marked removed + added rather than passing as unchanged.
    const out = buildRedlineHast(md("see [docs](a.md)"), md("see [docs](b.md)"));
    expect(summarize(out)).toEqual(["del:see docs", "ins:see docs"]);
  });

  it("an inline emphasis change is NOT silently unchanged", () => {
    const out = buildRedlineHast(md("really **important**"), md("really important"));
    expect(summarize(out)).toEqual(["del:really important", "ins:really important"]);
  });

  it("an inserted duplicate heading does NOT phantom-mark untouched blocks", () => {
    // rehype-slug renumbers duplicate-text heading ids (s, s-1, s-2…); inserting
    // one shifts later suffixes. The signature drops `id`, so a pure insertion
    // marks nothing as removed.
    const before = "## S\n\na\n\n## S\n\nb";
    const after = "## S\n\nintro\n\n## S\n\na\n\n## S\n\nb";
    const out = summarize(buildRedlineHast(md(before), md(after)));
    expect(out.filter((s) => s.startsWith("del:"))).toEqual([]);
  });
});

describe("surfaceFrontmatter (SW-GS-3)", () => {
  it("converts a leading frontmatter block to a yaml code fence", () => {
    expect(surfaceFrontmatter("---\nstatus: todo\n---\n\nbody")).toBe(
      "```yaml\nstatus: todo\n```\n\nbody",
    );
  });

  it("leaves a doc without frontmatter unchanged", () => {
    expect(surfaceFrontmatter("# hi\n\nbody")).toBe("# hi\n\nbody");
  });

  it("makes a frontmatter-only change visible in the redline", () => {
    const out = summarize(
      buildRedlineHast(
        md(surfaceFrontmatter("---\nstatus: todo\n---\n\nbody")),
        md(surfaceFrontmatter("---\nstatus: done\n---\n\nbody")),
      ),
    );
    expect(out.some((s) => s.startsWith("del:") && s.includes("todo"))).toBe(true);
    expect(out.some((s) => s.startsWith("ins:") && s.includes("done"))).toBe(true);
  });
});
