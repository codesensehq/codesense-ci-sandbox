// SPDX-License-Identifier: MIT
import { Extension } from "@tiptap/core";
import type { Node as PMNode } from "@tiptap/pm/model";
import { Plugin, PluginKey, type EditorState } from "@tiptap/pm/state";
import { Decoration, DecorationSet } from "@tiptap/pm/view";
import { diffArrays } from "diff";

import { openTasksByHeading } from "./note-blocks";
import { diffAgainst, docLines, markdownLines } from "./note-diff";

/** What the pane asks the editor to show of an agent's change: the body before it,
 *  and whether the review (green "added" tint) or the quieter "changed" tint shows. */
export interface ChangeOverlay {
  baseline: string;
  review: boolean;
  removedInPlace?: boolean;
}

interface NoteDecoState {
  overlay: ChangeOverlay | null;
  deco: DecorationSet;
  removed: string[];
}

export const noteDecorationsKey = new PluginKey<NoteDecoState>("noteDecorations");

function countWidget(count: number): HTMLElement {
  const el = document.createElement("span");
  el.className = "nt-heading-count";
  el.textContent = String(count);
  el.setAttribute("aria-label", `${count} open`);
  el.contentEditable = "false";
  return el;
}

function build(
  doc: PMNode,
  overlay: ChangeOverlay | null,
): Pick<NoteDecoState, "deco" | "removed"> {
  let removed: string[] = [];
  const decos: Decoration[] = [];
  for (const [pos, count] of openTasksByHeading(doc)) {
    if (count === 0) continue;
    const heading = doc.nodeAt(pos);
    if (!heading) continue;
    decos.push(
      Decoration.widget(pos + heading.nodeSize - 1, () => countWidget(count), {
        side: 1,
        key: `count-${count}`,
        ignoreSelection: true,
      }),
    );
  }
  if (overlay) {
    const cls = overlay.review ? "nt-chg nt-chg-add" : "nt-chg";
    const diff = diffAgainst(doc, overlay.baseline);
    removed = diff.removed;
    for (const line of diff.changed) {
      decos.push(Decoration.node(line.pos, line.pos + line.node.nodeSize, { class: cls }));
    }
    if (overlay.review && overlay.removedInPlace) {
      const lines = docLines(doc);
      let index = 0;
      for (const part of diffArrays(
        markdownLines(overlay.baseline),
        lines.map((line) => line.text),
      )) {
        if (!part.removed) {
          index += part.value.length;
          continue;
        }
        const pos = lines[index]?.pos ?? doc.content.size;
        part.value.forEach((text, offset) => {
          decos.push(
            Decoration.widget(
              pos,
              () => {
                const el = document.createElement("div");
                el.className = "nt-removed";
                el.textContent = text;
                el.contentEditable = "false";
                return el;
              },
              { side: -1, key: `removed-${pos}-${offset}-${text}`, ignoreSelection: true },
            ),
          );
        });
      }
    }
  }
  return { deco: DecorationSet.create(doc, decos), removed };
}

/** Open-task counts beside headings, and the tint over lines an agent changed. */
export const NoteDecorations = Extension.create({
  name: "noteDecorations",
  addProseMirrorPlugins() {
    return [
      new Plugin<NoteDecoState>({
        key: noteDecorationsKey,
        state: {
          init: (_config, state: EditorState) => ({
            overlay: null,
            ...build(state.doc, null),
          }),
          apply: (tr, prev, _old, state) => {
            const meta = tr.getMeta(noteDecorationsKey) as
              | { overlay: ChangeOverlay | null }
              | undefined;
            const overlay = meta ? meta.overlay : prev.overlay;
            if (!meta && !tr.docChanged) return prev;
            return { overlay, ...build(state.doc, overlay) };
          },
        },
        props: {
          decorations: (state) => noteDecorationsKey.getState(state)?.deco ?? DecorationSet.empty,
        },
      }),
    ];
  },
});
