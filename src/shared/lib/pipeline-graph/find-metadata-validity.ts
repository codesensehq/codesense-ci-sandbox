// SPDX-License-Identifier: MIT
import { findNavNodeByPath } from "./find-nav-node-by-path";
import type { DerivedPipelineTree } from "./types";

export interface MetadataValidity {
  metadataValid: boolean;
  metadataInvalidReason?: string | null;
}

/**
 * SLW-MD-1: look up the `metadataValid` flag for the open path from the scanned
 * nav tree (StoryRef or DocRef), so the viewer's Metadata chip dot mirrors the
 * nav badge from a single source of truth. Returns `null` when the path isn't
 * a known story/doc row (e.g. a generic file) — the chip defaults to valid.
 */
export function findMetadataValidity(
  tree: DerivedPipelineTree | null,
  filePath: string,
): MetadataValidity | null {
  const match = findNavNodeByPath(tree, filePath);
  if (!match) return null;
  const node = match.story ?? match.doc;
  if (!node) return null;
  return {
    metadataValid: node.metadataValid ?? true,
    metadataInvalidReason: node.metadataInvalidReason,
  };
}
