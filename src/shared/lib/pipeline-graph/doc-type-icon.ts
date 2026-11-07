// SPDX-License-Identifier: MIT
import { Bug, Lightbulb, FileText, History, BookOpen, Shapes, Image } from "lucide-react";

/** Icon, tooltip, and color for each document type. Colors are locked
 *  (not theme-dependent) so type identity reads consistently across themes. */
export const DOC_TYPE_ICON: Record<
  string,
  { icon: React.ElementType; label: string; color: string }
> = {
  story: { icon: BookOpen, label: "Story", color: "#6366f1" },
  bug: { icon: Bug, label: "Bug", color: "#f43f5e" },
  spike: { icon: Lightbulb, label: "Spike", color: "#14b8a6" },
  doc: { icon: FileText, label: "Document", color: "#64748b" },
  retro: { icon: History, label: "Retrospective", color: "#64748b" },
  // SW-SC-2: `.excalidraw` supporting docs. `Shapes` matches the file-tree
  // icon chosen for `.excalidraw` in pipeline-status-nav (SW-SC-1).
  canvas: { icon: Shapes, label: "Canvas", color: "#8b5cf6" },
  // SW-SC-7: images as first-class supporting docs.
  image: { icon: Image, label: "Image", color: "#0ea5e9" },
};
