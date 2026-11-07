// SPDX-License-Identifier: MIT
import { memo, useDeferredValue } from "react";
import ReactMarkdown from "react-markdown";

import { VIEWER_REMARK_PLUGINS, VIEWER_REHYPE_PLUGINS } from "@/shared/lib/markdown-render-plugins";

// ---------------------------------------------------------------------------
// SW-VE-10 — the live-preview pane. A lightweight, read-only rendered projection
// of the CONTEXT EDITOR's live buffer, shown beside the raw CM6 editor.
//
// Why this is NOT the heavy read-only `MarkdownViewer`:
//   1. `MarkdownViewer` sources its content from `filePath` via `readFile` +
//      `fileContentCache` — it renders the SAVED file, so it cannot show the
//      in-progress buffer without polluting the shared cache (which the editor
//      seed + other viewers read). This pane takes the buffer as a `content`
//      prop instead — raw bytes stay canonical IN THE EDITOR; the preview is a
//      pure, throwaway projection.
//   2. `MarkdownViewer`'s incremental machinery (a markdown Web Worker, morphdom
//      DOM patching, `createRoot` diagram/csquery islands, mermaid) is tuned for
//      viewing large saved files — overkill and a per-keystroke cost for a live
//      preview. The synchronous `react-markdown` render is bounded (the inset
//      only mounts on non-oversized files).
//
// Non-blocking: this pane owns its own `useDeferredValue`, so it renders preview
// updates at LOW priority and yields to keystrokes — the CM6 editor never blocks
// on the markdown parse. Because the caller mounts the pane ONLY while the preview
// is open+active, the deferred hook doesn't even run when hidden (zero work), and
// on mount `useDeferredValue` returns the current buffer immediately (instant
// open, no blank flash). The expensive `<ReactMarkdown>` render lives in a body
// memoized on the deferred value, so an unchanged value never re-parses.
//
// The plugin pipeline is the SHARED `VIEWER_*_PLUGINS` set, so the preview shows
// the exact markdown flavour the read-only viewer does (GFM, frontmatter parsed
// and hidden, GitHub alerts, highlighted code fences).
// ---------------------------------------------------------------------------

interface MarkdownPreviewPaneProps {
  /** The live editor buffer (raw markdown source incl. frontmatter). Fed directly
   *  — the pane defers it internally, so the caller passes the buffer as-is. */
  content: string;
  /** Mirror `MarkdownViewer`'s theme rule: the `default` viewer theme inverts
   *  prose in app-dark; the explicit themes carry their own palette. */
  proseInvert: boolean;
}

/** The actual markdown render — memoized on the (deferred) content so an
 *  unchanged value never re-parses. Frontmatter is consumed by
 *  `remark-frontmatter` and renders nothing (body prose only, never a second
 *  frontmatter surface). */
const MarkdownPreviewBody = memo(function MarkdownPreviewBody({
  content,
  proseInvert,
}: MarkdownPreviewPaneProps) {
  return (
    <div
      data-testid="markdown-preview-pane"
      aria-label="Rendered preview"
      className="h-full min-h-0 overflow-auto"
    >
      <div className="mx-auto max-w-none px-6 py-4">
        <div
          className={`md-viewer-prose prose prose-sm max-w-none ${
            proseInvert ? "dark:prose-invert" : ""
          }`}
        >
          <ReactMarkdown
            remarkPlugins={VIEWER_REMARK_PLUGINS}
            rehypePlugins={VIEWER_REHYPE_PLUGINS}
          >
            {content}
          </ReactMarkdown>
        </div>
      </div>
    </div>
  );
});

/** Read-only rendered markdown of the live buffer. Defers the buffer internally
 *  (non-blocking, low-priority) and delegates the parse to a memoized body. */
export function MarkdownPreviewPane({ content, proseInvert }: MarkdownPreviewPaneProps) {
  const deferred = useDeferredValue(content);
  return <MarkdownPreviewBody content={deferred} proseInvert={proseInvert} />;
}
