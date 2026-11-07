// SPDX-License-Identifier: MIT
import { diffArrays } from "diff";
import type { Element, Root as HastRoot, RootContent } from "hast";

/**
 * SW-GS-3 - the rendered-markdown redline core (the shared SW-DR-2 spine).
 *
 * Given two already-parsed HAST roots (the `before` = publish-baseline doc and
 * the `after` = working-tree doc), block-align their top-level blocks and emit
 * one HAST root where removed blocks are wrapped in `<del class="cs-redline-del">`
 * and added blocks in `<ins class="cs-redline-add">`. Unchanged blocks pass
 * through untouched.
 *
 * Granularity is line/block-level: alignment is over a per-block structural
 * signature (tag + properties + text). Intra-block word-level diffing is
 * intentionally deferred - SW-DR-2 extends THIS module with `diffWordsWithSpace`
 * inside each paired del/add rather than building a second redline path
 * (README D5: "don't build redline twice").
 *
 * Pure + synchronous so it runs inside the markdown web worker (off the UI
 * thread) and is unit-testable without a DOM.
 */

/** Collect the significant top-level blocks, dropping inter-block whitespace
 *  text nodes that rehype leaves between elements. */
function topLevelBlocks(root: HastRoot): RootContent[] {
  return root.children.filter(
    (c) => c.type === "element" || (c.type === "text" && c.value.trim() !== ""),
  );
}

/**
 * Structural signature used to align blocks. Captures the full rendered shape -
 * tag name, element properties, and text - recursively, NOT just concatenated
 * text. This way an inline-only change that leaves the text identical (a link
 * `href` swap, `**bold**` to plain, an inline-code to plain) still reads as a
 * changed block (removed + added) instead of silently passing as unchanged.
 * Word-level highlighting WITHIN a changed block is deferred to SW-DR-2.
 */
function blockSignature(node: RootContent): string {
  if (node.type === "text") return node.value;
  if (node.type === "element") {
    const el = node as Element;
    // Drop `id`: rehype-slug derives it from document-global slugger state
    // (duplicate headings get `-1`/`-2` suffixes), so inserting one duplicate
    // would shift later ids and phantom-mark untouched blocks. Every other
    // property (href, src, highlight classes) is block-content-derived, so it
    // stays — that is what catches inline-only changes.
    const { id: _id, ...props } = el.properties ?? {};
    return `<${el.tagName} ${JSON.stringify(props)}>${el.children
      .map((c) => blockSignature(c as RootContent))
      .join("")}`;
  }
  return "";
}

/**
 * Surface a leading YAML frontmatter block as a fenced ```yaml code block so it
 * is VISIBLE in the redline. `remark-rehype` otherwise drops frontmatter, which
 * would make a frontmatter-only change (e.g. a story `status:` transition)
 * render an empty, markless redline. Applied to both sides before parsing.
 */
export function surfaceFrontmatter(text: string): string {
  const m = text.match(/^---\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n)*/);
  if (!m) return text;
  const yaml = m[1];
  const body = text.slice(m[0].length);
  return "```yaml\n" + yaml + "\n```\n\n" + body;
}

function wrap(node: RootContent, tagName: "ins" | "del", className: string): Element {
  return {
    type: "element",
    tagName,
    properties: { className: [className] },
    children: [node as Element],
  };
}

/**
 * Build the annotated redline HAST root from before/after HAST roots.
 * Added-whole-doc -> every block wrapped `ins`; deleted-whole-doc -> every block
 * wrapped `del`.
 */
export function buildRedlineHast(beforeRoot: HastRoot, afterRoot: HastRoot): HastRoot {
  const beforeBlocks = topLevelBlocks(beforeRoot);
  const afterBlocks = topLevelBlocks(afterRoot);
  const beforeSigs = beforeBlocks.map(blockSignature);
  const afterSigs = afterBlocks.map(blockSignature);

  const parts = diffArrays(beforeSigs, afterSigs);
  const children: RootContent[] = [];
  let bi = 0;
  let ai = 0;
  for (const part of parts) {
    const n = part.count ?? part.value.length;
    if (part.added) {
      for (let k = 0; k < n; k++) children.push(wrap(afterBlocks[ai++], "ins", "cs-redline-add"));
    } else if (part.removed) {
      for (let k = 0; k < n; k++) children.push(wrap(beforeBlocks[bi++], "del", "cs-redline-del"));
    } else {
      // Unchanged: emit the after-side block; advance both cursors.
      for (let k = 0; k < n; k++) {
        children.push(afterBlocks[ai++]);
        bi++;
      }
    }
  }
  return { type: "root", children };
}
