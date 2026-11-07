// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { Editor } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import { Markdown } from "tiptap-markdown";
import { csLinkOptions } from "./cs-link";
import { insertNoteLink, isLinkPickerAllowedAt } from "./note-link-suggestion";
import { serializeWithBlankLines, type MarkdownDocSerializer } from "./scratch-blank-lines";
import type { PickerItem } from "./note-link-picker-items";

function makeEditor(content: string) {
  return new Editor({
    extensions: [StarterKit.configure({ link: csLinkOptions }), Markdown],
    content,
  });
}
function serialize(editor: Editor): string {
  const s = (editor.storage as unknown as { markdown: { serializer: MarkdownDocSerializer } })
    .markdown.serializer;
  return serializeWithBlankLines(editor.state.doc, editor.schema, s);
}

const storyItem: PickerItem = {
  key: "story:SW-SN-2",
  label: "The rail",
  kind: "story",
  link: { kind: "story", target: "SW-SN-2" },
};

describe("insertNoteLink (SW-SN-9 Task 2)", () => {
  it("inserts a cs: link that serializes to markdown and does not linkify the trailing text", () => {
    const editor = makeEditor("<p>see </p>"); // positions: 'see ' → cursor at end = 5
    insertNoteLink(editor, { from: 5, to: 5 }, storyItem);

    const html = editor.getHTML();
    expect(html).toContain('href="cs:story:SW-SN-2"');
    expect(html).toContain(">The rail</a>");

    // A trailing unmarked space follows, so continued typing is NOT linked.
    editor.commands.insertContent("more");
    expect(editor.getHTML()).not.toContain(">The railmore</a>");

    expect(serialize(editor)).toContain("[The rail](cs:story:SW-SN-2)");
    editor.destroy();
  });

  it("inserts a space-containing file target as a percent-encoded, round-tripping link", () => {
    const fileItem: PickerItem = {
      key: "file:x",
      label: "My Doc",
      kind: "file",
      link: { kind: "file", target: "ux/My Doc.md" },
    };
    const editor = makeEditor("<p></p>");
    insertNoteLink(editor, { from: 1, to: 1 }, fileItem);
    expect(editor.getHTML()).toContain('href="cs:file:ux/My%20Doc.md"');
    const first = serialize(makeEditor(serialize(editor)));
    expect(first).toContain("cs:file:ux/My%20Doc.md");
    expect(serialize(makeEditor(first))).toBe(first); // idempotent
    editor.destroy();
  });

  it("the inserted link round-trips through reload (Task 1 + Task 2)", () => {
    const editor = makeEditor("<p></p>");
    insertNoteLink(editor, { from: 1, to: 1 }, storyItem);

    // First reload normalizes the incidental trailing space; the cs: link is
    // preserved and every subsequent reload is then a fixed point (idempotent).
    const first = serialize(makeEditor(serialize(editor)));
    const second = serialize(makeEditor(first));
    expect(second).toBe(first);
    expect(first).toContain("[The rail](cs:story:SW-SN-2)");
    editor.destroy();
  });
});

describe("isLinkPickerAllowedAt (SW-SN-9 code suppression)", () => {
  it("is allowed in prose but NOT inside a code block", () => {
    const editor = makeEditor("<p>a</p><pre><code>b</code></pre>");
    const { doc } = editor.state;
    // pos 1 → inside the paragraph; the code block's text sits after it.
    expect(isLinkPickerAllowedAt(doc, 1)).toBe(true);
    let codePos = -1;
    doc.descendants((node, pos) => {
      if (node.type.name === "codeBlock") codePos = pos + 1;
    });
    expect(codePos).toBeGreaterThan(0);
    expect(isLinkPickerAllowedAt(doc, codePos)).toBe(false);
    editor.destroy();
  });

  it("is NOT allowed inside an inline code mark (text meant literally)", () => {
    const editor = makeEditor("<p>a<code>bcd</code>e</p>");
    const { doc } = editor.state;
    // Find a position within the inline-code text span.
    let codeMarkPos = -1;
    doc.descendants((node, pos) => {
      if (node.isText && node.marks.some((m) => m.type.spec.code)) codeMarkPos = pos + 1;
    });
    expect(codeMarkPos).toBeGreaterThan(0);
    expect(isLinkPickerAllowedAt(doc, codeMarkPos)).toBe(false);
    editor.destroy();
  });
});
