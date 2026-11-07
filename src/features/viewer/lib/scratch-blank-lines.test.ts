// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { restoreBlankParagraphs } from "./scratch-blank-lines";

// Load-side (parse-IN) counterpart to serializeWithBlankLines: surplus blank
// lines in a raw markdown string become NBSP lines so they survive the load into
// the editor as visible blank paragraphs. This is the fix for blank lines
// vanishing on notes written as plain markdown (the SW-SN-7 agent, external
// edits, or bodies authored before the serialize-out fix shipped).

const NBSP = " ";

describe("restoreBlankParagraphs", () => {
  it("leaves a single blank line (a normal paragraph separator) untouched", () => {
    expect(restoreBlankParagraphs("a\n\nb")).toBe("a\n\nb");
  });

  it("turns one surplus blank line (a double blank) into an NBSP paragraph", () => {
    // `a\n\n\nb` = separator + 1 empty paragraph → the empty paragraph is kept.
    expect(restoreBlankParagraphs("a\n\n\nb")).toBe(`a\n\n${NBSP}\n\nb`);
  });

  it("turns two surplus blank lines into two NBSP paragraphs", () => {
    expect(restoreBlankParagraphs("a\n\n\n\nb")).toBe(`a\n\n${NBSP}\n\n${NBSP}\n\nb`);
  });

  it("is idempotent — a second pass is a no-op (NBSP lines are not blank)", () => {
    const once = restoreBlankParagraphs("a\n\n\nb");
    expect(restoreBlankParagraphs(once)).toBe(once);
  });

  it("matches the serialize-out shape so the round-trip is stable", () => {
    // serializeWithBlankLines emits `a\n\n<NBSP>\n\nb` for one interior blank;
    // feeding that back through restore must not change it.
    const serialized = `a\n\n${NBSP}\n\nb`;
    expect(restoreBlankParagraphs(serialized)).toBe(serialized);
  });

  it("never rewrites blank lines inside a fenced code block", () => {
    const md = "```\nconst a = 1;\n\n\nconst b = 2;\n```";
    // Blanks inside the fence are literal code — passed through verbatim.
    expect(restoreBlankParagraphs(md)).toBe(md);
    expect(restoreBlankParagraphs(md)).not.toContain(NBSP);
  });

  it("keeps blank lines inside a tilde fence verbatim (interior untouched)", () => {
    const md = "~~~\ncode\n\n\ncode\n~~~\n\n\nafter";
    // Fence interior passes through; the run after the closing fence is adjacent
    // to a structural line (the closer), so it collapses rather than expands.
    expect(restoreBlankParagraphs(md)).toBe("~~~\ncode\n\n\ncode\n~~~\n\nafter");
  });

  it("does not let a shorter inner fence close a longer opening fence", () => {
    // 4-backtick opener; an inner 3-backtick line does NOT close it (CommonMark).
    const md = "````\ncode\n```\nstill code\n\n\nstill code\n````\n\n\nafter";
    const result = restoreBlankParagraphs(md);
    // Everything up to the 4-backtick closer is literal code → no NBSP injected.
    expect(result).toContain("```\nstill code\n\n\nstill code");
    expect(result.split("````")[1]).not.toContain(NBSP);
  });

  it("does not treat a 4-space-indented ``` as a fence (indented code)", () => {
    // An indented ``` is indented code, not a fence — it must not open a fence
    // that would swallow the rest of the note.
    const md = "    ```\ntext\n\n\nmore";
    // `text`/`more` are plain and flank a double blank → still expanded.
    expect(restoreBlankParagraphs(md)).toContain(`text\n\n${NBSP}\n\nmore`);
  });

  it("collapses (does not expand) leading and trailing blank runs at note edges", () => {
    expect(restoreBlankParagraphs("\n\nhi")).toBe("\nhi");
    expect(restoreBlankParagraphs("hi\n\n")).toBe("hi\n");
  });

  it("tolerates CRLF line endings (feature still active)", () => {
    const result = restoreBlankParagraphs("a\r\n\r\n\r\nb");
    expect(result).toContain(NBSP);
  });

  it("returns empty / single-line input unchanged", () => {
    expect(restoreBlankParagraphs("")).toBe("");
    expect(restoreBlankParagraphs("just one line")).toBe("just one line");
  });
});

// Structure safety: the parse-in transform must never inject an NBSP paragraph
// into or beside a list, blockquote, heading, or indented code block — doing so
// splits the block and corrupts it permanently on disk.
describe("restoreBlankParagraphs — structure safety", () => {
  it("does not split a loose list (double blank between items)", () => {
    const md = "- a\n\n\n- b";
    const result = restoreBlankParagraphs(md);
    expect(result).toBe("- a\n\n- b");
    expect(result).not.toContain(NBSP);
  });

  it("does not split an ordered list", () => {
    const result = restoreBlankParagraphs("1. a\n\n\n2. b");
    expect(result).not.toContain(NBSP);
  });

  it("does not inject into a blockquote boundary", () => {
    const result = restoreBlankParagraphs("> quoted\n\n\ntext after");
    expect(result).not.toContain(NBSP);
  });

  it("does not split an indented code block", () => {
    const result = restoreBlankParagraphs("    code line\n\n\n    more code");
    expect(result).not.toContain(NBSP);
  });

  it("does not expand a run adjacent to a heading", () => {
    const result = restoreBlankParagraphs("# Heading\n\n\nbody");
    expect(result).not.toContain(NBSP);
  });

  it("does not split a multi-paragraph (loose) list item — indented continuation", () => {
    // A list-item continuation paragraph is indented 1-3 spaces and marker-less;
    // a column-0 NBSP injected between two of them would terminate the list.
    const bullet = restoreBlankParagraphs("- item\n\n  more\n\n\n  even more");
    expect(bullet).not.toContain(NBSP);
    const ordered = restoreBlankParagraphs("1. item\n\n   more\n\n\n   even more");
    expect(ordered).not.toContain(NBSP);
  });

  it("does not expand a blank run before a fence that carries an info string", () => {
    const result = restoreBlankParagraphs("para\n\n\n```js\ncode\n```");
    expect(result).not.toContain(NBSP);
  });

  it("still expands a double blank between two ordinary paragraphs", () => {
    // The core case must keep working after the structure guard.
    expect(restoreBlankParagraphs("para one\n\n\npara two")).toBe(
      `para one\n\n${NBSP}\n\npara two`,
    );
  });
});
