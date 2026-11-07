// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import { Editor } from "@tiptap/core";
import { createLowlight, all } from "lowlight";
import {
  noteEditorExtensions,
  serializeEditorMarkdown,
  loadMarkdown,
} from "../editor/note-editor-extensions";
import { agentRefsInMarkdown, formatAgentRef } from "./agent-ref";
import {
  appendAgentRef,
  blockAtPos,
  openTasksByHeading,
  relocateBlock,
  sendTargetOf,
} from "./note-blocks";
import { diffAgainst } from "./note-diff";
import { noteDecorationsKey } from "./note-decorations";

// The notes editor's markdown contract, through a REAL tiptap + tiptap-markdown
// editor: checklists and handoff chips must survive save → load → save unchanged.

let editors: Editor[] = [];
afterEach(() => {
  for (const e of editors) e.destroy();
  editors = [];
});

function makeEditor(markdown: string): Editor {
  const editor = new Editor({
    extensions: noteEditorExtensions({
      lowlight: createLowlight(all),
      suggestions: {
        link: { getItems: () => [], onChange: () => {}, onKeyDown: () => false },
        slash: { onChange: () => {}, onKeyDown: () => false },
      },
      keys: { send: () => false },
    }),
  });
  loadMarkdown(editor, markdown);
  editors.push(editor);
  return editor;
}

const save = serializeEditorMarkdown;

function posOfText(editor: Editor, text: string): number {
  let at = -1;
  editor.state.doc.descendants((node, pos) => {
    if (at < 0 && node.isText && node.text?.includes(text)) at = pos;
    return at < 0;
  });
  if (at < 0) throw new Error(`no text ${text}`);
  return at;
}

const NOTE = `## This week

- [ ] Remove agent accounts [@Jimmy](cs:agent:BUG-142%23dev)
- [ ] Redesign the drop down
- [x] Messaging human`;

describe("checklists", () => {
  it("parse `- [ ]` / `- [x]` into task items and save them back the same", () => {
    const editor = makeEditor(NOTE);
    const types: string[] = [];
    editor.state.doc.descendants((n) => {
      types.push(n.type.name);
      return true;
    });
    expect(types).toContain("taskList");
    expect(types.filter((t) => t === "taskItem")).toHaveLength(3);
    const saved = save(editor);
    expect(saved).toContain("- [ ] Redesign the drop down");
    expect(saved).toContain("- [x] Messaging human");
    expect(save(makeEditor(saved))).toBe(saved);
  });

  it("count the open tasks under each heading", () => {
    const editor = makeEditor(NOTE);
    expect([...openTasksByHeading(editor.state.doc).values()]).toEqual([2]);
  });

  it("show the open-task count beside the heading", () => {
    const editor = makeEditor(NOTE);
    const deco = noteDecorationsKey.getState(editor.state)?.deco;
    const widgets = deco?.find() ?? [];
    expect(widgets).toHaveLength(1);
  });
});

describe("handoff chips (the `[@Name](cs:agent:handle)` link)", () => {
  it("load as an agentRef atom and save back as the same readable link", () => {
    const editor = makeEditor(NOTE);
    let ref: { handle: string; name: string } | null = null;
    editor.state.doc.descendants((n) => {
      if (n.type.name === "agentRef") ref = n.attrs as { handle: string; name: string };
      return true;
    });
    expect(ref).toEqual({ handle: "BUG-142#dev", name: "Jimmy" });
    expect(save(editor)).toContain("- [ ] Remove agent accounts [@Jimmy](cs:agent:BUG-142%23dev)");
  });

  it("send the line's words, never the chip", () => {
    const editor = makeEditor(NOTE);
    editor.commands.setTextSelection(posOfText(editor, "Remove") + 2);
    expect(sendTargetOf(editor)?.text).toBe("Remove agent accounts");
  });

  it("are added once per agent at the end of a handed-off line", () => {
    const editor = makeEditor(NOTE);
    const block = blockAtPos(editor.state.doc, posOfText(editor, "Redesign") + 1);
    expect(block?.node.type.name).toBe("taskItem");
    expect(appendAgentRef(editor, block!.pos, { handle: "BUG-151#spec", name: "Spec" })).toBe(true);
    expect(appendAgentRef(editor, block!.pos, { handle: "BUG-151#spec", name: "Spec" })).toBe(
      false,
    );
    const saved = save(editor);
    expect(saved).toContain(
      `- [ ] Redesign the drop down ${formatAgentRef({ handle: "BUG-151#spec", name: "Spec" })}`,
    );
    expect(agentRefsInMarkdown(saved).map((r) => r.handle)).toEqual([
      "BUG-142#dev",
      "BUG-151#spec",
    ]);
  });
});

describe("finding a sent line again after the note changed", () => {
  it("follows the line when text is added above it, and gives up when it is gone", () => {
    const editor = makeEditor("first\n\nsecond\n\nthird\n");
    const block = blockAtPos(editor.state.doc, posOfText(editor, "second") + 1)!;
    // Typing a new line above shifts every position below it.
    editor.commands.insertContentAt(0, "<p>above</p>");
    const now = editor.state.doc.nodeAt(block.pos);
    expect(now?.type).toBe(block.node.type);
    expect(now?.textContent).not.toBe("second");
    const pos = relocateBlock(editor.state.doc, block);
    expect(pos).not.toBeNull();
    expect(editor.state.doc.nodeAt(pos!)?.textContent).toBe("second");

    editor.commands.setContent("<p>first</p><p>third</p>");
    expect(relocateBlock(editor.state.doc, block)).toBeNull();
  });
});

describe("what an agent changed", () => {
  it("names the new lines and the removed ones against the earlier body", () => {
    const before = `- [ ] Remove agent accounts [@Jimmy](cs:agent:BUG-142%23dev)
- the drop down needs a redesign
- [x] Messaging human`;
    const editor = makeEditor(NOTE);
    const diff = diffAgainst(editor.state.doc, before);
    expect(diff.changed.map((l) => l.text)).toEqual(["This week", "Redesign the drop down"]);
    expect(diff.removed).toEqual(["the drop down needs a redesign"]);
  });

  it("tints the changed lines while an overlay is set, and clears it", () => {
    const editor = makeEditor(NOTE);
    const count = () => noteDecorationsKey.getState(editor.state)?.deco.find().length ?? 0;
    const base = count();
    editor.view.dispatch(
      editor.state.tr.setMeta(noteDecorationsKey, {
        overlay: { baseline: "- [ ] Remove agent accounts", review: false },
      }),
    );
    expect(count()).toBeGreaterThan(base);
    editor.view.dispatch(editor.state.tr.setMeta(noteDecorationsKey, { overlay: null }));
    expect(count()).toBe(base);
  });
});
