// SPDX-License-Identifier: MIT
import { Extension, type Editor, type NodeViewRenderer, type EditorOptions } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import Placeholder from "@tiptap/extension-placeholder";
import CodeBlockLowlight from "@tiptap/extension-code-block-lowlight";
import Typography from "@tiptap/extension-typography";
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import { Markdown } from "tiptap-markdown";
import { AgentRef } from "../lib/agent-ref-node";
import { NoteDecorations } from "../lib/note-decorations";
import { createSlashCommands, type SlashMenuController } from "../lib/slash-commands";
import { csLinkOptions } from "../../viewer/lib/cs-link";
import {
  createNoteLinkExtension,
  type NoteLinkSuggestionController,
} from "../../viewer/lib/note-link-suggestion";
import {
  restoreBlankParagraphs,
  serializeWithBlankLines,
  type MarkdownDocSerializer,
} from "../../viewer/lib/scratch-blank-lines";

export interface NoteEditorOptions {
  lowlight: NonNullable<Parameters<typeof CodeBlockLowlight.configure>[0]>["lowlight"];
  placeholder?: string;
  nodeViews?: { agentRef?: NodeViewRenderer; codeBlock?: NodeViewRenderer };
  suggestions: { link: NoteLinkSuggestionController; slash: SlashMenuController };
  keys: { send(paste: boolean): boolean; pasteAsCode?(editor: Editor): boolean };
}

/** Minimal editor shape for Tab handlers — avoids coupling tests to the full Editor class */
export interface TabHandlerEditor {
  isActive(name: string): boolean;
  state: { selection: { from: number; to: number }; tr: TabHandlerTransaction };
  view: { dispatch(tr: TabHandlerTransaction): void };
}

interface TabHandlerTransaction {
  insertText(text: string, from: number, to: number): TabHandlerTransaction;
}

/** Tab handler: insert spaces in code block, insert tab in prose, or
 *  defer to the list/task item's own sink handler for lists. */
export function handleScratchNoteTab(editor: TabHandlerEditor): boolean {
  if (editor.isActive("codeBlock")) {
    const { from, to } = editor.state.selection;
    editor.view.dispatch(editor.state.tr.insertText("  ", from, to));
    return true;
  }
  // In a list or checklist: return false so the item's own Tab handler
  // (sinkListItem) fires — it knows the correct node type.
  if (editor.isActive("listItem") || editor.isActive("taskItem")) {
    return false;
  }
  // Prose: bypass tiptap-markdown's insertContent override by dispatching
  // a ProseMirror transaction directly — same pattern as handlePaste.
  const { from, to } = editor.state.selection;
  editor.view.dispatch(editor.state.tr.insertText("\t", from, to));
  return true;
}

/** Shift-Tab handler: defer to the list/task item for lists, no-op elsewhere. */
export function handleScratchNoteShiftTab(editor: TabHandlerEditor): boolean {
  if (editor.isActive("codeBlock")) {
    return true;
  }
  if (editor.isActive("listItem") || editor.isActive("taskItem")) {
    return false;
  }
  return true;
}

export function noteEditorExtensions(opts: NoteEditorOptions) {
  return [
    StarterKit.configure({ codeBlock: false, link: csLinkOptions }),
    CodeBlockLowlight.extend({
      addNodeView() {
        return opts.nodeViews?.codeBlock ?? null;
      },
    }).configure({ lowlight: opts.lowlight, defaultLanguage: "plaintext" }),
    TaskList,
    TaskItem.configure({ nested: true }),
    AgentRef.extend({
      addNodeView() {
        return opts.nodeViews?.agentRef ?? null;
      },
    }),
    NoteDecorations,
    Placeholder.configure({ placeholder: opts.placeholder ?? "Type, or press / for blocks" }),
    Typography,
    Markdown.configure({ transformPastedText: true }),
    Extension.create({
      name: "scratchNoteShortcuts",
      addKeyboardShortcuts() {
        return {
          "Mod-Enter": () => opts.keys.send(false),
          "Mod-Alt-Enter": () => opts.keys.send(true),
          "Mod-Shift-v": ({ editor }) => {
            if (opts.keys.pasteAsCode) return opts.keys.pasteAsCode(editor);
            navigator.clipboard.readText().then((text) => {
              if (text) editor.chain().focus().setCodeBlock().insertContent(text).run();
            });
            return true;
          },
          "Mod-Ctrl-1": ({ editor }) => editor.chain().focus().toggleHeading({ level: 1 }).run(),
          "Mod-Ctrl-2": ({ editor }) => editor.chain().focus().toggleHeading({ level: 2 }).run(),
          "Mod-Ctrl-3": ({ editor }) => editor.chain().focus().toggleHeading({ level: 3 }).run(),
          Tab: ({ editor }) => handleScratchNoteTab(editor),
          "Shift-Tab": ({ editor }) => handleScratchNoteShiftTab(editor),
        };
      },
    }),
    createNoteLinkExtension(opts.suggestions.link),
    createSlashCommands(opts.suggestions.slash),
  ];
}

export function noteEditorProps({
  onLinkClick,
}: {
  onLinkClick(href: string): boolean;
}): EditorOptions["editorProps"] {
  return {
    attributes: { class: "scratch-note-editor", spellcheck: "false", "aria-label": "Note" },
    handleClick(_view, _pos, event) {
      const href = (event.target as HTMLElement | null)?.closest?.("a")?.getAttribute("href");
      return href ? onLinkClick(href) : false;
    },
    handlePaste(view, event) {
      if (!view.state.selection.$from.parent.type.spec.code) return false;
      const text = event.clipboardData?.getData("text/plain");
      if (!text) return false;
      view.dispatch(view.state.tr.insertText(text));
      return true;
    },
  };
}

export function serializeEditorMarkdown(editor: Editor): string {
  const serializer = (
    editor.storage as unknown as { markdown: { serializer: MarkdownDocSerializer } }
  ).markdown.serializer;
  return serializeWithBlankLines(editor.state.doc, editor.schema, serializer);
}

export function loadMarkdown(
  editor: Editor,
  markdown: string,
  options: { addToHistory?: false } = {},
): void {
  const content = restoreBlankParagraphs(markdown);
  if (options.addToHistory === false) {
    editor.chain().setMeta("addToHistory", false).setContent(content, { emitUpdate: false }).run();
  } else {
    editor.commands.setContent(content, { emitUpdate: false });
  }
}
