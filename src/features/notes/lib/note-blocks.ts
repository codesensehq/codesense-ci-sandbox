// SPDX-License-Identifier: MIT
import type { Editor } from "@tiptap/core";
import type { Node as PMNode } from "@tiptap/pm/model";

import type { AgentRefAttrs } from "./agent-ref";

/** A line of the note that can be handed to an agent: a list or task item (its own
 *  line, not its nested children), a paragraph or heading, or a whole code block. */
export interface NoteBlock {
  /** Document position of the block node (before it). */
  pos: number;
  node: PMNode;
  /** The line's text as it would be sent (handoff chips left out). */
  text: string;
}

const LIST_ITEMS = new Set(["taskItem", "listItem"]);

/** The text of a textblock without its inline atoms (the handoff chips). */
export function lineText(node: PMNode): string {
  return node.textBetween(0, node.content.size, "\n", () => "").trim();
}

/** The block a document position sits in, innermost list item first. */
export function blockAtPos(doc: PMNode, pos: number): NoteBlock | null {
  const clamped = Math.max(0, Math.min(pos, doc.content.size));
  const $pos = doc.resolve(clamped);
  for (let depth = $pos.depth; depth > 0; depth--) {
    const node = $pos.node(depth);
    if (node.type.name === "codeBlock") {
      return { pos: $pos.before(depth), node, text: node.textContent };
    }
    if (LIST_ITEMS.has(node.type.name)) {
      const first = node.firstChild;
      return { pos: $pos.before(depth), node, text: first ? lineText(first) : "" };
    }
  }
  if ($pos.depth >= 1) {
    const node = $pos.node(1);
    if (node.isTextblock) return { pos: $pos.before(1), node, text: lineText(node) };
  }
  return null;
}

/** A block's text the way `blockAtPos` reports it. */
function blockText(node: PMNode): string {
  if (node.type.name === "codeBlock") return node.textContent;
  if (LIST_ITEMS.has(node.type.name)) return node.firstChild ? lineText(node.firstChild) : "";
  return lineText(node);
}

/** Where `block` is in `doc` now, after edits that may have moved it: its old
 *  position when the same line is still there, else the one block of its kind with
 *  its text. `null` when the line is gone or the text no longer names one line. */
export function relocateBlock(doc: PMNode, block: NoteBlock): number | null {
  const same = (node: PMNode | null | undefined) =>
    !!node && node.type === block.node.type && blockText(node) === block.text;
  if (block.pos <= doc.content.size && same(doc.nodeAt(block.pos))) return block.pos;
  const found: number[] = [];
  doc.descendants((node, pos) => {
    if (same(node)) found.push(pos);
    return found.length < 2;
  });
  return found.length === 1 ? found[0] : null;
}

/** What ⌘↵ sends: the selected text when there is a selection, else the line the
 *  caret is on. `block` is set when a whole line goes, so a handoff chip can be
 *  added to it. */
export function sendTargetOf(editor: Editor): { text: string; block: NoteBlock | null } | null {
  const { state } = editor;
  const { from, to } = state.selection;
  if (from !== to) {
    const text = state.doc.textBetween(from, to, "\n", () => "").trim();
    return text ? { text, block: null } : null;
  }
  const block = blockAtPos(state.doc, from);
  if (!block || !block.text) return null;
  return { text: block.text, block };
}

/** The textblock a handoff chip goes at the end of: the block itself, or a list
 *  item's own line. Returns its position and node. */
function lineOf(doc: PMNode, blockPos: number): { pos: number; node: PMNode } | null {
  const block = doc.nodeAt(blockPos);
  if (!block) return null;
  if (block.isTextblock) return { pos: blockPos, node: block };
  if (LIST_ITEMS.has(block.type.name) && block.firstChild?.isTextblock) {
    return { pos: blockPos + 1, node: block.firstChild };
  }
  return null;
}

/** Whether the line already carries a chip for this agent. */
function hasRef(line: PMNode, handle: string): boolean {
  let found = false;
  line.forEach((child) => {
    if (child.type.name === "agentRef" && child.attrs.handle === handle) found = true;
  });
  return found;
}

/** Add a handoff chip for `agent` at the end of the line at `blockPos` (once per
 *  agent). Code blocks carry no chips. Returns whether one was added. */
export function appendAgentRef(editor: Editor, blockPos: number, agent: AgentRefAttrs): boolean {
  const line = lineOf(editor.state.doc, blockPos);
  if (!line || line.node.type.spec.code || hasRef(line.node, agent.handle)) return false;
  const type = editor.schema.nodes.agentRef;
  if (!type) return false;
  const end = line.pos + line.node.nodeSize - 1;
  const lastChar = line.node.textContent.slice(-1);
  const tr = editor.state.tr;
  const parts: PMNode[] = [];
  if (line.node.content.size > 0 && lastChar !== " ") parts.push(editor.schema.text(" "));
  parts.push(type.create(agent));
  tr.insert(end, parts);
  editor.view.dispatch(tr);
  return true;
}

/** The number of unchecked tasks under each heading (until the next heading of the
 *  same or a higher level), keyed by the heading's position. */
export function openTasksByHeading(doc: PMNode): Map<number, number> {
  const headings: { pos: number; level: number }[] = [];
  const counts = new Map<number, number>();
  doc.descendants((node, pos) => {
    if (node.type.name === "heading") {
      const level = node.attrs.level as number;
      while (headings.length && headings[headings.length - 1].level >= level) headings.pop();
      headings.push({ pos, level });
      counts.set(pos, 0);
      return false;
    }
    if (node.type.name === "taskItem" && !node.attrs.checked) {
      for (const h of headings) counts.set(h.pos, (counts.get(h.pos) ?? 0) + 1);
    }
    return true;
  });
  return counts;
}
