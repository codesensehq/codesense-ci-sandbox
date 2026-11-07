// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { Editor } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import { Markdown } from "tiptap-markdown";
import {
  serializeWithBlankLines,
  withPreservedBlankParagraphs,
  restoreBlankParagraphs,
  type MarkdownDocSerializer,
} from "./scratch-blank-lines";

// SW-SN-8 AC-8: blank-line preservation, verified through a REAL tiptap +
// tiptap-markdown editor (not a mock). This is the layer that matters — the
// serializer drops empty paragraphs, so the fix must operate on the doc and be
// proven against the actual serializer, which these tests do.

const NBSP = "\u00A0";

function makeEditor(content = "") {
  return new Editor({ extensions: [StarterKit, Markdown], content });
}

function serializer(editor: Editor): MarkdownDocSerializer {
  return (editor.storage as unknown as { markdown: { serializer: MarkdownDocSerializer } }).markdown
    .serializer;
}

function serialize(editor: Editor): string {
  return serializeWithBlankLines(editor.state.doc, editor.schema, serializer(editor));
}

function plainMarkdown(editor: Editor): string {
  return (
    editor.storage as unknown as { markdown: { getMarkdown: () => string } }
  ).markdown.getMarkdown();
}

describe("serializeWithBlankLines — real editor (SW-SN-8 AC-8)", () => {
  it("proves the premise: plain getMarkdown DROPS empty paragraphs", () => {
    const editor = makeEditor("<p>a</p><p></p><p></p><p>b</p>");
    // This is WHY the naive string transform failed — the blank lines are
    // already gone by the time getMarkdown returns.
    expect(plainMarkdown(editor)).toBe("a\n\nb");
    editor.destroy();
  });

  it("preserves intentional blank lines as NBSP paragraphs", () => {
    const editor = makeEditor("<p>a</p><p></p><p></p><p>b</p>");
    const stored = serialize(editor);
    // The empty paragraphs now survive as NBSP lines.
    expect(stored).toContain(NBSP);
    expect(stored).not.toBe("a\n\nb");
    editor.destroy();
  });

  it("round-trips: a spaced note survives save→reload→save unchanged", () => {
    const editor = makeEditor("<p>a</p><p></p><p></p><p>b</p>");
    const stored = serialize(editor);

    // Reload the stored markdown into a fresh editor and re-serialize.
    const reloaded = makeEditor(stored);
    expect(serialize(reloaded)).toBe(stored);

    editor.destroy();
    reloaded.destroy();
  });

  it("leaves normal single-paragraph-break content untouched", () => {
    const editor = makeEditor("<p>one</p><p>two</p>");
    expect(serialize(editor)).toBe("one\n\ntwo");
    editor.destroy();
  });

  it("does not inject NBSP into blank lines inside a code block", () => {
    const editor = makeEditor("<pre><code>const a = 1;\n\n\nconst b = 2;</code></pre>");
    const stored = serialize(editor);
    // The code interior keeps its real blank lines and gains no NBSP.
    expect(stored).toContain("const a = 1;\n\n\nconst b = 2;");
    expect(stored).not.toContain(NBSP);
    editor.destroy();
  });

  it("withPreservedBlankParagraphs is idempotent on the doc", () => {
    const editor = makeEditor("<p>x</p><p></p><p>y</p>");
    const once = withPreservedBlankParagraphs(editor.state.doc, editor.schema);
    const twice = withPreservedBlankParagraphs(once, editor.schema);
    expect(serializer(editor).serialize(twice)).toBe(serializer(editor).serialize(once));
    editor.destroy();
  });

  it("a blank/cleared note round-trips to empty (no phantom NBSP, placeholder intact)", () => {
    const editor = makeEditor("");
    expect(serialize(editor)).toBe("");
    const editor2 = makeEditor("<p></p>");
    expect(serialize(editor2)).toBe("");
    editor.destroy();
    editor2.destroy();
  });

  it("an NBSP-only note (text deleted, sentinels left) collapses to empty", () => {
    // After deleting the visible text but leaving prior blank-line sentinels, the
    // doc's only content is NBSP paragraphs — it must still round-trip to "".
    const editor = makeEditor(`<p>${NBSP}</p><p>${NBSP}</p>`);
    expect(serialize(editor)).toBe("");
    editor.destroy();
  });

  it("does not collapse a whitespace-only code block (structure is not 'blank')", () => {
    const editor = makeEditor("");
    const { schema } = editor;
    // A doc whose only content is a whitespace-filled code block must be kept,
    // never dropped to "" by the blank-note guard (that would lose the block).
    const doc = schema.node("doc", null, [schema.node("codeBlock", null, [schema.text("   ")])]);
    const stored = serializeWithBlankLines(doc, schema, serializer(editor));
    expect(stored).not.toBe("");
    expect(stored).toContain("```");
    editor.destroy();
  });

  it("collapses a single trailing empty paragraph (the fresh cursor line)", () => {
    const editor = makeEditor("<p>a</p><p></p>");
    // Trailing blank last line is incidental → collapses, no persistent NBSP.
    const stored = serialize(editor);
    expect(stored).toBe("a");
    expect(stored).not.toContain(NBSP);
    editor.destroy();
  });

  it("preserves a LEADING empty paragraph (top-of-note spacing)", () => {
    const editor = makeEditor("<p></p><p>a</p>");
    const stored = serialize(editor);
    // A blank line the user put above their text is intentional — kept as NBSP.
    expect(stored).toBe(`${NBSP}\n\na`);
    editor.destroy();
  });

  it("keeps all but the last of multiple trailing empty paragraphs", () => {
    const editor = makeEditor("<p>a</p><p></p><p></p>");
    // Two trailing blanks: the surplus one is intentional spacing (NBSP), the
    // final one is the cursor line (collapses).
    const stored = serialize(editor);
    expect(stored).toBe(`a\n\n${NBSP}`);
    editor.destroy();
  });

  it("agent-style plain-markdown blank lines survive load via restoreBlankParagraphs", () => {
    // The bug: a note written as PLAIN markdown by another writer (the SW-SN-7
    // agent, an external edit, or a pre-fix body) loses its blank lines because
    // markdown-it drops empty paragraphs on the way IN.
    const plain = "line one\n\n\nline two";

    // Without the parse-in fix, the blank line is gone the moment it loads.
    const withoutFix = makeEditor(plain);
    expect(serialize(withoutFix)).toBe("line one\n\nline two");
    withoutFix.destroy();

    // Running the raw string through restoreBlankParagraphs first preserves it.
    const withFix = makeEditor(restoreBlankParagraphs(plain));
    expect(serialize(withFix)).toBe(`line one\n\n${NBSP}\n\nline two`);
    withFix.destroy();
  });

  it("does not NBSP-ify empty paragraphs nested in a blockquote/list (top-level only)", () => {
    const editor = makeEditor("");
    const { schema } = editor;
    // A blockquote holding a text paragraph + a nested empty paragraph.
    const nestedDoc = schema.node("doc", null, [
      schema.node("blockquote", null, [
        schema.node("paragraph", null, [schema.text("q")]),
        schema.node("paragraph"),
      ]),
    ]);
    const patched = withPreservedBlankParagraphs(nestedDoc, schema);
    // The blockquote's inner empty paragraph must remain empty (not gain an NBSP).
    const innerEmpty = patched.child(0).child(1);
    expect(innerEmpty.type.name).toBe("paragraph");
    expect(innerEmpty.content.size).toBe(0);
    editor.destroy();
  });

  it("does NBSP-ify a top-level empty paragraph between content", () => {
    const editor = makeEditor("");
    const { schema } = editor;
    const doc = schema.node("doc", null, [
      schema.node("paragraph", null, [schema.text("a")]),
      schema.node("paragraph"),
      schema.node("paragraph", null, [schema.text("b")]),
    ]);
    const patched = withPreservedBlankParagraphs(doc, schema);
    // The middle top-level empty paragraph now carries an NBSP (content.size 1).
    expect(patched.child(1).content.size).toBe(1);
    expect(patched.child(1).textContent).toBe(NBSP);
    editor.destroy();
  });
});

// SW-SN-8 AC-2: markdown input rules still fire after the toolbar was removed
// (proving they were never toolbar-gated). Exercised through a REAL editor by
// simulating text input the way ProseMirror processes typed characters.
describe("markdown input rules survive toolbar removal (SW-SN-8 AC-2)", () => {
  type TextInputHandler = (view: Editor["view"], from: number, to: number, text: string) => boolean;

  function typeText(editor: Editor, text: string) {
    const { view } = editor;
    for (const ch of text) {
      const { from, to } = view.state.selection;
      const handled = view.someProp("handleTextInput", (f) =>
        (f as TextInputHandler)(view, from, to, ch),
      );
      if (!handled) view.dispatch(view.state.tr.insertText(ch, from, to));
    }
  }

  it("`# ` converts the line to an H1", () => {
    const editor = makeEditor("<p></p>");
    editor.commands.focus();
    typeText(editor, "# Title");
    expect(editor.isActive("heading", { level: 1 })).toBe(true);
    editor.destroy();
  });

  it("`**x**` produces bold text", () => {
    const editor = makeEditor("<p></p>");
    editor.commands.focus();
    typeText(editor, "**bold**");
    expect(editor.getHTML()).toContain("<strong>");
    editor.destroy();
  });

  it("`- ` starts a bullet list", () => {
    const editor = makeEditor("<p></p>");
    editor.commands.focus();
    typeText(editor, "- item");
    expect(editor.isActive("bulletList")).toBe(true);
    editor.destroy();
  });
});
