// SPDX-License-Identifier: MIT
// @vitest-environment jsdom
import { readFileSync, readdirSync } from "node:fs";
import { afterEach, expect, it } from "vitest";
import { createEditor } from "./kit-entry";
import { Editor } from "@tiptap/core";
import { all, createLowlight } from "lowlight";
import {
  noteEditorExtensions,
  serializeEditorMarkdown,
  loadMarkdown,
} from "./note-editor-extensions";
const lowlight = createLowlight(all);
lowlight.register("plaintext", () => ({ contains: [] }));
const editors: Editor[] = [];
afterEach(() => editors.splice(0).forEach((e) => e.destroy()));
function native(md: string) {
  const editor = new Editor({
    extensions: noteEditorExtensions({
      lowlight,
      suggestions: {
        link: { getItems: () => [], onChange: () => {}, onKeyDown: () => false },
        slash: { onChange: () => {}, onKeyDown: () => false },
      },
      keys: { send: () => false },
    }),
  });
  loadMarkdown(editor, md);
  editors.push(editor);
  return editor;
}
const save = serializeEditorMarkdown;
// Native normalizes prose tabs to spaces on load; the typography fixture pins that behavior.
const dir = `${process.cwd()}/src/features/notes/editor/__golden__/`;
for (const name of readdirSync(dir).filter(
  (n) => n.endsWith(".md") && !n.endsWith(".expected.md"),
)) {
  it(`native Markdown bytes: ${name}`, () => {
    const md = readFileSync(`${dir}${name}`, "utf8");
    const saved = save(native(md));
    const expectedName = name.replace(/(?:\.input)?\.md$/, ".expected.md");
    const expected = readdirSync(dir).includes(expectedName)
      ? readFileSync(`${dir}${expectedName}`, "utf8")
      : md;
    expect(saved).toBe(expected);
    expect(save(native(expected))).toBe(expected);
    const element = document.createElement("div");
    document.body.append(element);
    const kit = createEditor(element, { markdown: md });
    try {
      expect(kit.getMarkdown()).toBe(expected);
      kit.setMarkdown(expected, { emit: false });
      expect(kit.getMarkdown()).toBe(expected);
    } finally {
      kit.destroy();
      element.remove();
    }
  });
}
