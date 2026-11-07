// SPDX-License-Identifier: MIT
export { CodeEditor } from "./CodeEditor";
export type { CodeEditorProps, CodeEditorHandle, CursorInfo } from "./CodeEditor";
export { useCodeEditor } from "./use-code-editor";
export { CodeMergeView } from "./CodeMergeView";
export type { CodeMergeViewProps, CodeMergeViewHandle } from "./CodeMergeView";
export { CodeDiffReview } from "./CodeDiffReview";
export type { CodeDiffReviewProps, DiffNavigator, ChunkPosition } from "./CodeDiffReview";
export { useCodeMergeView } from "./use-code-merge-view";
export { decodeUtf8 } from "./cm-loader";
export type { LanguageId, CmModule } from "./cm-loader";
export { detectLanguage, detectLanguageId, buildLanguageExtension } from "./language-detection";
export { isStructuralLanguage } from "./structural-diff";
export { useEditorAffordances } from "./use-editor-affordances";
export type { ResolvedEditorAffordances } from "./editor-affordances";
export { languageLabel } from "./language-detection";
export {
  detectLineEnding,
  detectIndentation,
  lineEndingLabel,
  indentationLabel,
} from "./text-format";
export type { LineEnding, Indentation } from "./text-format";
export { computeLineChanges, summarizeLineChanges } from "./line-changes";
export type { LineChange, LineChangeSummary } from "./line-changes";
