// SPDX-License-Identifier: MIT
import { Extension } from "@tiptap/core";
import type { Editor, Range } from "@tiptap/core";
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";
import Suggestion from "@tiptap/suggestion";
import { pickerItemHref, type PickerItem } from "./note-link-picker-items";

/** The `[[` picker is only valid where a link mark is meaningful — i.e. NOT in a
 *  code block (whose schema rejects marks; a selection there would insert plain
 *  text and silently drop the link) and NOT inside an inline `code` mark (where
 *  text is meant literally, mirroring SW-SN-8's code-is-literal treatment).
 *  Exported for unit coverage of the `allow`. */
export function isLinkPickerAllowedAt(doc: ProseMirrorNode, pos: number): boolean {
  const $pos = doc.resolve(pos);
  if ($pos.parent.type.spec.code) return false;
  return !$pos.marks().some((m) => m.type.spec.code);
}

/** SW-SN-9 Task 2: the `[[` autocomplete, built on `@tiptap/suggestion`. The
 *  extension is caret/query tracking only — the popup UI + selection live in
 *  React (the panel owns them), bridged through a small controller so the
 *  once-created editor closure always sees live data. */

/** Live picker state the popup renders from. `clientRect` is the caret rect
 *  getter (re-read on scroll); `select` commits the chosen item. */
export interface NoteLinkPickerState {
  items: PickerItem[];
  clientRect: () => DOMRect | null;
  select: (item: PickerItem) => void;
}

export interface NoteLinkSuggestionController {
  /** Current candidates for `query` (notes + warm-tree docs, filtered). */
  getItems: (query: string) => PickerItem[];
  /** Push popup state to React (or `null` to close). */
  onChange: (state: NoteLinkPickerState | null) => void;
  /** Suggestion forwards editor keystrokes here so arrow/enter/escape drive the
   *  list without stealing editor focus; returns true when the key was handled.
   *  (A function, not a ref, so the panel can defer to the popup's live handler
   *  without exposing a ref that's read during render.) */
  onKeyDown: (event: KeyboardEvent) => boolean;
}

/** Replace the `[[query` range with a link mark spanning the label, then an
 *  UNMARKED trailing space so the next keystroke isn't part of the link. Goes
 *  through `editor.commands` (chain) — never a raw transaction — so it respects
 *  the Markdown `insertContent` override. */
export function insertNoteLink(editor: Editor, range: Range, item: PickerItem): void {
  const href = pickerItemHref(item);
  editor
    .chain()
    .focus()
    .insertContentAt(range, [
      { type: "text", text: item.label, marks: [{ type: "link", attrs: { href } }] },
      { type: "text", text: " " },
    ])
    .run();
}

export function createNoteLinkExtension(controller: NoteLinkSuggestionController): Extension {
  return Extension.create({
    name: "noteLinkPicker",
    addProseMirrorPlugins() {
      return [
        Suggestion<PickerItem, PickerItem>({
          editor: this.editor,
          char: "[[",
          startOfLine: false,
          // Titles and file names contain spaces — keep the query open across
          // them (a picker filter, not an identifier).
          allowSpaces: true,
          allow: ({ state, range }) => isLinkPickerAllowedAt(state.doc, range.from),
          items: ({ query }) => controller.getItems(query),
          command: ({ editor, range, props }) => insertNoteLink(editor, range, props),
          render: () => {
            const push = (props: {
              items: PickerItem[];
              command: (item: PickerItem) => void;
              clientRect?: (() => DOMRect | null) | null;
            }) =>
              controller.onChange({
                items: props.items,
                clientRect: props.clientRect ?? (() => null),
                select: (item) => props.command(item),
              });
            return {
              onStart: push,
              onUpdate: push,
              onKeyDown: (props) => controller.onKeyDown(props.event),
              onExit: () => controller.onChange(null),
            };
          },
        }),
      ];
    },
  });
}
