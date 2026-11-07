// SPDX-License-Identifier: MIT
// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { Editor } from "@tiptap/core";
import { createLowlight } from "lowlight";
import {
  noteEditorExtensions,
  noteEditorProps,
  loadMarkdown,
  serializeEditorMarkdown,
} from "./note-editor-extensions";
const editors: Editor[] = [];
afterEach(() => editors.splice(0).forEach((e) => e.destroy()));
function mount(md: string) {
  const send = vi.fn((_paste: boolean) => true);
  const editor = new Editor({
    extensions: noteEditorExtensions({
      lowlight: createLowlight(),
      keys: { send },
      suggestions: {
        link: { getItems: () => [], onChange: () => {}, onKeyDown: () => false },
        slash: { onChange: () => {}, onKeyDown: () => false },
      },
    }),
    editorProps: noteEditorProps({ onLinkClick: () => false }),
  });
  editors.push(editor);
  loadMarkdown(editor, md);
  return { editor, send };
}
function key(editor: Editor, key: string, extra: KeyboardEventInit = {}) {
  editor.view.dom.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...extra }),
  );
}
it("routes send shortcuts and keeps Tab literal in prose and two spaces in code", () => {
  const { editor, send } = mount("Hello");
  key(editor, "Enter", { metaKey: true });
  key(editor, "Enter", { metaKey: true, altKey: true });
  expect(send.mock.calls).toEqual([[false], [true]]);
  key(editor, "Tab");
  expect(serializeEditorMarkdown(editor)).toContain("Hello\t");
  loadMarkdown(editor, "```plaintext\nx\n```");
  key(editor, "Tab");
  expect(serializeEditorMarkdown(editor)).toBe("```plaintext\nx  \n```");
});
it("nests task items with Tab and outdents with Shift-Tab", () => {
  const { editor } = mount("- [ ] First\n\n- [ ] Second");
  let pos = 0;
  editor.state.doc.descendants((node, at) => {
    if (node.text === "Second") pos = at;
  });
  editor.commands.setTextSelection(pos);
  key(editor, "Tab");
  expect(serializeEditorMarkdown(editor)).toContain("  - [ ] Second");
  key(editor, "Tab", { shiftKey: true });
  expect(serializeEditorMarkdown(editor)).not.toContain("  - [ ] Second");
});
it("pastes code literally instead of parsing Markdown", () => {
  const { editor } = mount("```plaintext\nx\n```");
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: { getData: () => "**literal**\n\tcode", files: [], types: ["text/plain"] },
  });
  editor.view.dom.dispatchEvent(event);
  expect(serializeEditorMarkdown(editor)).toContain("x**literal**\n\tcode");
});
