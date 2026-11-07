// SPDX-License-Identifier: MIT
import { defaultForegroundLayout, effectiveMosaicRoot } from "@/shared/stores/viewer-store";
import type { MosaicNode } from "./mosaic-tree";

/** Leaf widths of the real mosaic arrangement for `n` tiles in a region. */
export function tileWidths(n: number, preset: "columns" | "grid", regionWidth: number): number[] {
  const ids = Array.from({ length: n }, (_, i) => `w${i}`);
  const root = effectiveMosaicRoot(defaultForegroundLayout({ preset }), ids);
  const out: number[] = [];
  const walk = (node: MosaicNode | null, width: number) => {
    if (!node) return;
    if (node.kind === "leaf") {
      out.push(width);
      return;
    }
    node.children.forEach((c, i) =>
      walk(c, node.axis === "row" ? (width * node.sizes[i]) / 100 : width),
    );
  };
  walk(root, regionWidth);
  return out;
}
