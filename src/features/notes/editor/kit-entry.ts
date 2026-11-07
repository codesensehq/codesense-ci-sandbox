// SPDX-License-Identifier: MIT
import { Editor } from "@tiptap/core";
import { common, createLowlight } from "lowlight";
import { exitSuggestion, SuggestionPluginKey } from "@tiptap/suggestion";
import {
  noteEditorExtensions,
  noteEditorProps,
  serializeEditorMarkdown,
  loadMarkdown,
} from "./note-editor-extensions";
import { domNodeViews, type DomViewOptions } from "./dom-node-views";
import {
  enumerateHeadings,
  scrollToHeadingPos,
  type Heading,
} from "../../viewer/lib/note-headings";
import {
  computeSelectionMenu,
  readActiveMarks,
  type ActiveMarks,
  type SelectionMenuTarget,
} from "../../viewer/lib/scratch-selection";
import { insertNoteLink, type NoteLinkPickerState } from "../../viewer/lib/note-link-suggestion";
import type { PickerItem } from "../../viewer/lib/note-link-picker-items";
import { slashCommandsKey, SLASH_ITEMS, type SlashMenuState } from "../lib/slash-commands";
import {
  appendAgentRef,
  blockAtPos,
  relocateBlock,
  sendTargetOf,
  type NoteBlock,
} from "../lib/note-blocks";
import type { AgentRefAttrs } from "../lib/agent-ref";
import { noteDecorationsKey, type ChangeOverlay } from "../lib/note-decorations";

declare const __NOTE_EDITOR_VERSION__: string;
const lowlight = createLowlight(common);
lowlight.register("plaintext", () => ({ contains: [] }));
export const version =
  typeof __NOTE_EDITOR_VERSION__ === "undefined" ? "development" : __NOTE_EDITOR_VERSION__;
export type SelectionInfo = SelectionMenuTarget & { rect: DOMRect; marks: ActiveMarks };
type MenuState<T> = T & { selectedIndex: number; setIndex(index: number): void };
export interface KitEditorOptions extends DomViewOptions {
  markdown?: string;
  placeholder?: string;
  readOnly?: boolean;
  scrollContainer?: HTMLElement;
  onChange?(markdown: string): void;
  onSelection?(info: SelectionInfo | null): void;
  onHeadings?(headings: Heading[]): void;
  onSend?(paste: boolean): boolean | void;
  onLinkClick?(href: string): void;
  suggestions?: {
    link?: {
      items(query: string): PickerItem[];
      onState?(state: MenuState<NoteLinkPickerState> | null): void;
    };
    slash?: { onState?(state: MenuState<SlashMenuState> | null): void };
  };
}

function menuState<T extends { items: unknown[] }>(
  state: T | null,
  selectedIndex: number,
  setIndex: (index: number) => void,
): MenuState<T> | null {
  return state
    ? {
        ...state,
        selectedIndex,
        setIndex: (index) => setIndex(Math.max(0, Math.min(index, state.items.length - 1))),
      }
    : null;
}

/** A page-owned editor: data and actions arrive only through callbacks.
 * scrollToHeading scrolls scrollContainer when supplied, otherwise the root's
 * nearest scrollable ancestor (or the document scroll port). It scrolls only
 * that container; pages do not need the native host's .nt-scroll class.
 * blockAt returns the block's current document pos for appendAgentRef.
 */
export function createEditor(element: HTMLElement, options: KitEditorOptions = {}) {
  let nextPasteIsCode = false;
  let escapeTab = false;
  let quiet = false;
  let link: NoteLinkPickerState | null = null;
  let slash: SlashMenuState | null = null;
  let linkIndex = 0,
    slashIndex = 0;
  const views = domNodeViews(options);
  const baseProps = noteEditorProps({ onLinkClick: () => false });
  const pushLink = () =>
    options.suggestions?.link?.onState?.(
      menuState(link, linkIndex, (index) => {
        linkIndex = index;
        pushLink();
      }),
    );
  const pushSlash = () =>
    options.suggestions?.slash?.onState?.(
      menuState(slash, slashIndex, (index) => {
        slashIndex = index;
        pushSlash();
      }),
    );
  function menuKey(event: KeyboardEvent, kind: "link" | "slash"): boolean {
    const state = kind === "link" ? link : slash;
    if (event.key === "Escape") {
      exitSuggestion(editor.view, kind === "link" ? SuggestionPluginKey : slashCommandsKey);
      escapeTab = true;
      event.preventDefault();
      return true;
    }
    const menuWired =
      kind === "link" ? options.suggestions?.link?.onState : options.suggestions?.slash?.onState;
    if (!state || !state.items.length || !menuWired) return false;
    let index = kind === "link" ? linkIndex : slashIndex;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      index =
        (index + (event.key === "ArrowDown" ? 1 : -1) + state.items.length) % state.items.length;
      if (kind === "link") {
        linkIndex = index;
        pushLink();
      } else {
        slashIndex = index;
        pushSlash();
      }
    } else if (event.key === "Enter" || event.key === "Tab") {
      if (kind === "link" && link?.items[index]) link.select(link.items[index]);
      else if (kind === "slash" && slash?.items[index]) slash.select(slash.items[index]);
    } else return false;
    event.preventDefault();
    return true;
  }
  const selection = () => {
    const target = computeSelectionMenu(editor);
    const domSelection = element.ownerDocument.getSelection();
    const range = domSelection?.rangeCount ? domSelection.getRangeAt(0) : null;
    const rect =
      range && editor.view.dom.contains(range.commonAncestorContainer)
        ? range.getBoundingClientRect()
        : null;
    options.onSelection?.(
      target && rect ? { ...target, rect, marks: readActiveMarks(editor) } : null,
    );
  };
  const editor: Editor = new Editor({
    element,
    editable: !options.readOnly,
    extensions: noteEditorExtensions({
      lowlight,
      placeholder: options.placeholder,
      nodeViews: views,
      keys: {
        send: (paste) => {
          options.onSend?.(paste);
          return true;
        },
        pasteAsCode: () => {
          nextPasteIsCode = true;
          return false;
        },
      },
      suggestions: {
        link: {
          getItems: (q) => options.suggestions?.link?.items(q) ?? [],
          onChange: (state) => {
            link = state;
            linkIndex = 0;
            pushLink();
          },
          onKeyDown: (e) => menuKey(e, "link"),
        },
        slash: {
          onChange: (state) => {
            slash = state;
            slashIndex = 0;
            pushSlash();
          },
          onKeyDown: (e) => menuKey(e, "slash"),
        },
      },
    }),
    editorProps: {
      ...baseProps,
      // Architecture: pointer activation belongs to the DOM click; PM also calls handleClick on mouseup.
      handleClick: () => false,
      handleDOMEvents: {
        pointerdown: () => {
          escapeTab = false;
          return false;
        },
        keydown: (view, event) => {
          nextPasteIsCode = false;
          const escaped = escapeTab;
          // Shift belongs to the following Shift+Tab chord.
          if (event.key !== "Shift") escapeTab = false;
          if (
            event.isComposing ||
            event.metaKey ||
            event.ctrlKey ||
            event.altKey ||
            SuggestionPluginKey.getState(view.state)?.active ||
            slashCommandsKey.getState(view.state)?.active
          )
            return false;
          // ProseMirror's fallback captures every Esc. Let the genuine unclaimed
          // event bubble to the pane/shim, while active suggestions keep first refusal.
          if (event.key === "Escape" && !event.shiftKey) {
            escapeTab = true;
            return true;
          }
          // Skip ProseMirror's keymaps without preventing browser focus traversal.
          return (
            event.key === "Tab" &&
            (escaped ||
              (!editor.isActive("codeBlock") &&
                !editor.isActive("listItem") &&
                !editor.isActive("taskItem")))
          );
        },
        click: (_view, event) => {
          const anchor = (event.target as HTMLElement | null)?.closest?.("a");
          const href = anchor?.getAttribute("href");
          if (!anchor || !href || !editor.view.dom.contains(anchor)) return false;
          event.preventDefault();
          event.stopPropagation();
          options.onLinkClick?.(href);
          return true;
        },
      },
      handlePaste(view, event) {
        if (nextPasteIsCode) {
          nextPasteIsCode = false;
          const text = event.clipboardData?.getData("text/plain");
          if (text === undefined) return false;
          editor.commands.setCodeBlock();
          editor.view.dispatch(editor.state.tr.insertText(text));
          return true;
        }
        return baseProps.handlePaste?.(view, event, {} as never) ?? false;
      },
    },
    onUpdate: () => {
      if (!quiet) {
        options.onChange?.(serializeEditorMarkdown(editor));
        if (!editor.state.selection.empty) selection();
      }
    },
    onTransaction: ({ transaction }) => {
      if (transaction.docChanged && !quiet)
        options.onHeadings?.(enumerateHeadings(editor.state.doc));
    },
    onSelectionUpdate: () => {
      if (!quiet) selection();
    },
    onBlur: () => {
      nextPasteIsCode = false;
      escapeTab = false;
      options.onSelection?.(null);
    },
  });
  const setMarkdown = (md: string, _opts?: { emit?: false }) => {
    quiet = true;
    try {
      loadMarkdown(editor, md, { addToHistory: false });
    } finally {
      quiet = false;
    }
    options.onSelection?.(null);
    options.onHeadings?.(enumerateHeadings(editor.state.doc));
  };
  setMarkdown(options.markdown ?? "");
  return {
    getMarkdown: () => serializeEditorMarkdown(editor),
    setMarkdown,
    setEditable: (editable: boolean) => editor.setEditable(editable, false),
    focus: (at?: "start" | "end") => editor.commands.focus(at),
    destroy: () => editor.destroy(),
    getHeadings: () => enumerateHeadings(editor.state.doc),
    scrollToHeading: (slug: string) => {
      const h = enumerateHeadings(editor.state.doc).find((h) => h.slug === slug);
      if (!h) return;
      let container = options.scrollContainer;
      if (!container) {
        for (let parent = editor.view.dom.parentElement; parent; parent = parent.parentElement) {
          if (
            /(auto|scroll|overlay|hidden)/.test(getComputedStyle(parent).overflowY) &&
            parent.scrollHeight > parent.clientHeight
          ) {
            container = parent;
            break;
          }
        }
      }
      const port = container ?? (document.scrollingElement as HTMLElement);
      if (port === document.scrollingElement) {
        const dom = editor.view.nodeDOM(h.pos);
        if (dom instanceof HTMLElement) port.scrollTop += dom.getBoundingClientRect().top;
      } else {
        scrollToHeadingPos(editor, h.pos, port);
      }
    },
    blockAt: ({ x, y }: { x: number; y: number }) => {
      const coords = editor.view.posAtCoords({ left: x, top: y });
      const block = coords && blockAtPos(editor.state.doc, coords.pos);
      const dom = block && editor.view.nodeDOM(block.pos);
      return block && dom instanceof HTMLElement
        ? { ...block, rect: dom.getBoundingClientRect() }
        : null;
    },
    setChangeOverlay: (overlay: ChangeOverlay | null) => {
      editor.view.dispatch(
        editor.state.tr.setMeta(noteDecorationsKey, {
          overlay: overlay && { ...overlay, removedInPlace: true },
        }),
      );
      return { removed: noteDecorationsKey.getState(editor.state)?.removed ?? [] };
    },
    refreshAgents: views.refreshAgents,
    commands: {
      toggleBold: () => editor.chain().focus().toggleBold().run(),
      toggleItalic: () => editor.chain().focus().toggleItalic().run(),
      toggleCode: () => editor.chain().focus().toggleCode().run(),
      linkSelection: () => {
        const { from, to } = editor.state.selection;
        const text = editor.state.doc.textBetween(from, to, " ").trim();
        return (
          !!text &&
          editor
            .chain()
            .focus()
            .insertContentAt({ from, to }, "[[" + text)
            .run()
        );
      },
      setHeading: (n: 1 | 2 | 3 | 4 | 5 | 6) =>
        editor.chain().focus().setHeading({ level: n }).run(),
      insertSlashItem: (id: string) => {
        const item = SLASH_ITEMS.find((i) => i.id === id);
        if (!item) return false;
        if (slash) slash.select(item);
        else item.run(editor, editor.state.selection);
        return true;
      },
      insertLink: (item: PickerItem) => {
        if (link) link.select(item);
        else insertNoteLink(editor, editor.state.selection, item);
      },
      relocateBlock: (block: NoteBlock) => relocateBlock(editor.state.doc, block),
      appendAgentRef: (pos: number, agent: AgentRefAttrs) => appendAgentRef(editor, pos, agent),
      sendTarget: () => sendTargetOf(editor),
    },
    version,
  };
}
