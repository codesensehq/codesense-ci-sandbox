// SPDX-License-Identifier: MIT
import { Extension, type Editor, type Range } from "@tiptap/core";
import { PluginKey } from "@tiptap/pm/state";
import Suggestion from "@tiptap/suggestion";

/** A block the `/` menu inserts. */
export interface SlashItem {
  id: string;
  label: string;
  /** What to type for it instead (markdown shorthand or a key). */
  hint: string;
  keywords: string;
  run: (editor: Editor, range: Range) => void;
}

const chainAt = (editor: Editor, range: Range) => editor.chain().focus().deleteRange(range);

export const SLASH_ITEMS: readonly SlashItem[] = [
  {
    id: "task",
    label: "Checklist",
    hint: "- [ ]",
    keywords: "todo task check checkbox",
    run: (e, r) => chainAt(e, r).toggleTaskList().run(),
  },
  {
    id: "h1",
    label: "Heading",
    hint: "#",
    keywords: "title h1 heading",
    run: (e, r) => chainAt(e, r).setNode("heading", { level: 1 }).run(),
  },
  {
    id: "h2",
    label: "Subheading",
    hint: "##",
    keywords: "h2 section heading",
    run: (e, r) => chainAt(e, r).setNode("heading", { level: 2 }).run(),
  },
  {
    id: "h3",
    label: "Small heading",
    hint: "###",
    keywords: "h3 heading",
    run: (e, r) => chainAt(e, r).setNode("heading", { level: 3 }).run(),
  },
  {
    id: "bullets",
    label: "Bulleted list",
    hint: "-",
    keywords: "list bullet ul",
    run: (e, r) => chainAt(e, r).toggleBulletList().run(),
  },
  {
    id: "numbers",
    label: "Numbered list",
    hint: "1.",
    keywords: "list number ordered ol",
    run: (e, r) => chainAt(e, r).toggleOrderedList().run(),
  },
  {
    id: "code",
    label: "Code or prompt",
    hint: "```",
    keywords: "code prompt snippet block",
    run: (e, r) => chainAt(e, r).setCodeBlock().run(),
  },
  {
    id: "quote",
    label: "Quote",
    hint: ">",
    keywords: "quote blockquote",
    run: (e, r) => chainAt(e, r).toggleBlockquote().run(),
  },
  {
    id: "divider",
    label: "Divider",
    hint: "---",
    keywords: "divider rule hr line",
    run: (e, r) => chainAt(e, r).setHorizontalRule().run(),
  },
];

/** The items whose label or keywords start a word with `query`. */
export function filterSlashItems(query: string): SlashItem[] {
  const q = query.trim().toLowerCase();
  if (!q) return [...SLASH_ITEMS];
  return SLASH_ITEMS.filter((item) =>
    `${item.label} ${item.keywords}`
      .toLowerCase()
      .split(/\s+/)
      .some((word) => word.startsWith(q)),
  );
}

/** Live `/` menu state for the popup. */
export interface SlashMenuState {
  items: SlashItem[];
  clientRect: () => DOMRect | null;
  select: (item: SlashItem) => void;
}

export interface SlashMenuController {
  onChange: (state: SlashMenuState | null) => void;
  onKeyDown: (event: KeyboardEvent) => boolean;
}

/** The `/` menu: type `/` at the start of a line or after a space to insert a block. */
export const slashCommandsKey = new PluginKey("slashCommands");

export function createSlashCommands(controller: SlashMenuController): Extension {
  return Extension.create({
    name: "slashCommands",
    addProseMirrorPlugins() {
      return [
        Suggestion<SlashItem, SlashItem>({
          editor: this.editor,
          char: "/",
          pluginKey: slashCommandsKey,
          allow: ({ state, range }) => !state.doc.resolve(range.from).parent.type.spec.code,
          items: ({ query }) => filterSlashItems(query),
          command: ({ editor, range, props }) => props.run(editor, range),
          render: () => {
            const push = (props: {
              items: SlashItem[];
              command: (item: SlashItem) => void;
              clientRect?: (() => DOMRect | null) | null;
            }) =>
              controller.onChange(
                props.items.length
                  ? {
                      items: props.items,
                      clientRect: props.clientRect ?? (() => null),
                      select: (item) => props.command(item),
                    }
                  : null,
              );
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
