// SPDX-License-Identifier: MIT
import type { Node as PMNode } from "@tiptap/pm/model";

import { stripAgentRefs } from "./agent-ref";
import { lineText } from "./note-blocks";

/**
 * A line-level comparison of the note on screen against an earlier body, so the
 * pane can tint what an agent changed and list what it removed. Lines are compared
 * by their words (markdown syntax and handoff chips set aside), which keeps a moved
 * line from reading as changed and a re-checked task from reading as new text.
 */

/** A line's words: syntax, links and spacing set aside. */
export function normalizeLine(line: string): string {
  return stripAgentRefs(line)
    .replace(/\u00a0/g, " ")
    .replace(/^\s*(?:#{1,6}\s+|>\s*)/, "")
    .replace(/^\s*(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?/, "")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[*_`~]/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

/** The words of every line of a markdown body, code fences left out. */
export function markdownLines(markdown: string): string[] {
  const out: string[] = [];
  let inFence = false;
  for (const raw of markdown.split("\n")) {
    if (/^\s*(```|~~~)/.test(raw)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    const line = normalizeLine(raw);
    if (line) out.push(line);
  }
  return out;
}

/** A line of the document on screen: where it is and its words. */
export interface DocLine {
  /** Position of the node to tint (the list item for a list line). */
  pos: number;
  node: PMNode;
  text: string;
}

/** Every comparable line of the document: headings, paragraphs and list items. */
export function docLines(doc: PMNode): DocLine[] {
  const out: DocLine[] = [];
  doc.descendants((node, pos, parent) => {
    const name = node.type.name;
    if (name === "codeBlock") return false;
    if (name === "taskItem" || name === "listItem") {
      const first = node.firstChild;
      const text = first ? normalizeLine(lineText(first)) : "";
      if (text) out.push({ pos, node, text });
      return true;
    }
    if (node.isTextblock) {
      const inItem = parent && (parent.type.name === "taskItem" || parent.type.name === "listItem");
      if (!inItem) {
        const text = normalizeLine(lineText(node));
        if (text) out.push({ pos, node, text });
      }
      return false;
    }
    return true;
  });
  return out;
}

export interface NoteDiff {
  /** Lines on screen that the earlier body did not have. */
  changed: DocLine[];
  /** Lines the earlier body had that are gone now, in its order. */
  removed: string[];
}

export function diffAgainst(doc: PMNode, baselineMarkdown: string): NoteDiff {
  const before = markdownLines(baselineMarkdown);
  const beforeSet = new Set(before);
  const lines = docLines(doc);
  const nowSet = new Set(lines.map((l) => l.text));
  return {
    changed: lines.filter((l) => !beforeSet.has(l.text)),
    removed: before.filter((l) => !nowSet.has(l)),
  };
}
