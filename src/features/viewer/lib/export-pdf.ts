// SPDX-License-Identifier: MIT
/**
 * PDF export flow for HTML and markdown viewer tabs (R.4).
 *
 * Determines content type via getViewerFileType, builds the appropriate
 * content (URL for HTML, rendered HTML string for markdown), opens a
 * native save dialog, and dispatches to the Rust export command.
 */
import { save } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import {
  getViewerFileType,
  buildViewerUrl,
  stripViewerExtension,
} from "@/shared/lib/viewer-file-type";
import {
  exportPdfFromUrl,
  exportPdfFromHtml,
  getHttpApiPort,
  readFile,
  mintFrame,
  releaseFrame,
} from "@/tauri/commands";
import { parseToExportHtml } from "@/shared/lib/markdown-worker/client";
import { getErrorMessage } from "@/shared/lib/error-message";

/**
 * Inline print-friendly stylesheet for markdown PDF export.
 * Light background, dark text, readable code blocks, basic prose styling.
 */
const PRINT_STYLESHEET = `
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
    font-size: 14px;
    line-height: 1.6;
    color: #24292f;
    background: #fff;
    max-width: 800px;
    margin: 0 auto;
    padding: 2rem;
  }
  h1, h2, h3, h4, h5, h6 { margin-top: 1.5em; margin-bottom: 0.5em; }
  h1 { font-size: 2em; border-bottom: 1px solid #d0d7de; padding-bottom: 0.3em; }
  h2 { font-size: 1.5em; border-bottom: 1px solid #d0d7de; padding-bottom: 0.3em; }
  pre {
    background: #f6f8fa;
    border: 1px solid #d0d7de;
    border-radius: 6px;
    padding: 1em;
    overflow-x: auto;
    font-size: 13px;
    line-height: 1.45;
  }
  code { font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace; font-size: 0.9em; }
  :not(pre) > code { background: #f6f8fa; padding: 0.2em 0.4em; border-radius: 3px; }
  table { border-collapse: collapse; width: 100%; margin: 1em 0; }
  th, td { border: 1px solid #d0d7de; padding: 0.5em 1em; text-align: left; }
  th { background: #f6f8fa; font-weight: 600; }
  blockquote { border-left: 4px solid #d0d7de; margin: 1em 0; padding: 0.5em 1em; color: #57606a; }
  img { max-width: 100%; }
  a { color: #0969da; }
  hr { border: none; border-top: 1px solid #d0d7de; margin: 1.5em 0; }

  /* highlight.js GitHub Light theme — syntax coloring for exported code blocks */
  .hljs { color: #24292e; background: #ffffff; }
  .hljs-doctag, .hljs-keyword, .hljs-meta .hljs-keyword, .hljs-template-tag,
  .hljs-template-variable, .hljs-type, .hljs-variable.language_ { color: #d73a49; }
  .hljs-title, .hljs-title.class_, .hljs-title.class_.inherited__,
  .hljs-title.function_ { color: #6f42c1; }
  .hljs-attr, .hljs-attribute, .hljs-literal, .hljs-meta, .hljs-number,
  .hljs-operator, .hljs-variable, .hljs-selector-attr, .hljs-selector-class,
  .hljs-selector-id { color: #005cc5; }
  .hljs-regexp, .hljs-string, .hljs-meta .hljs-string { color: #032f62; }
  .hljs-built_in, .hljs-symbol { color: #e36209; }
  .hljs-comment, .hljs-code, .hljs-formula { color: #6a737d; }
  .hljs-name, .hljs-quote, .hljs-selector-tag, .hljs-selector-pseudo { color: #22863a; }
  .hljs-subst { color: #24292e; }
  .hljs-section { color: #005cc5; font-weight: bold; }
  .hljs-bullet { color: #735c0f; }
  .hljs-emphasis { color: #24292e; font-style: italic; }
  .hljs-strong { color: #24292e; font-weight: bold; }
  .hljs-addition { color: #22863a; background-color: #f0fff4; }
  .hljs-deletion { color: #b31d28; background-color: #ffeef0; }
`;

/**
 * Wrap an HTML fragment in a full document with inlined print stylesheet.
 */
function wrapExportHtml(htmlFragment: string): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <style>${PRINT_STYLESHEET}</style>
</head>
<body>${htmlFragment}</body>
</html>`;
}

/**
 * SW-VE-2: PDF export only supports the doc types this flow actually handles —
 * markdown (rendered via the export worker) and html (URL-served). It must NOT
 * be offered for `code`-type tabs (the markdown worker would mangle source: `#`,
 * `*`, backticks, inline `<...>` get interpreted) nor for images (which have no
 * export path and fall through the markdown branch to a garbage PDF). VE-2's
 * widening of `VIEWER_EXTENSIONS` made `isViewerFile` true for code exts, so the
 * affordance gates (viewer-tabs, shortcut registry) must use THIS predicate, not
 * `isViewerFile`, to stay scoped to exportable doc types.
 */
export function canExportToPdf(filePath: string): boolean {
  const type = getViewerFileType(filePath);
  return type === "markdown" || type === "html";
}

/**
 * Export the active viewer tab to PDF.
 *
 * @param filePath - Absolute path of the active tab file
 * @param teamContentPath - Per-team content root (post-PTS-2). Consumed by
 *   `buildViewerUrl` in the HTML branch as the base64-encoded URL root —
 *   must match the root the HTTP viewer endpoint expects, identical to
 *   what `HtmlViewer` uses for live rendering (SF-1 Bug C: prior to this
 *   parameter rename + caller update, exports of HTML files passed the
 *   legacy `contextRepoPath` and produced 404 / blank PDFs). The markdown
 *   branch ignores this parameter (it reads via `readFile`).
 */
export async function exportViewerToPdf(filePath: string, teamContentPath: string): Promise<void> {
  // Defense in depth: the affordance gates already hide this for non-doc tabs,
  // but guard the entry point too so a stale keybinding / future caller can't
  // push a code/config or image file through the markdown export worker.
  if (!canExportToPdf(filePath)) {
    toast.error("PDF export is only available for markdown and HTML files");
    return;
  }
  const fileName = filePath.split("/").pop() ?? "export";
  const suggestedName = `${stripViewerExtension(fileName)}.pdf`;

  // Open save dialog first — bail early if user cancels
  const savePath = await save({
    defaultPath: suggestedName,
    filters: [{ name: "PDF", extensions: ["pdf"] }],
  });
  if (!savePath) return;

  const toastId = toast.loading("Exporting PDF...");

  try {
    const fileType = getViewerFileType(filePath);

    if (fileType === "html") {
      const port = await getHttpApiPort();
      if (!port) throw new Error("HTTP API port not available");
      const frame = await mintFrame({
        documentPath: filePath,
        contentRoot: teamContentPath,
        slot: "export",
      });
      try {
        const url = buildViewerUrl(port, teamContentPath, filePath, { origin: frame.origin });
        await exportPdfFromUrl(url, savePath);
      } finally {
        await releaseFrame(frame.frameId);
      }
    } else {
      // markdown — render to HTML via the web worker export mode
      const text = await readFile(filePath);
      const { promise } = parseToExportHtml(text);
      const htmlFragment = await promise;
      const fullHtml = wrapExportHtml(htmlFragment);
      await exportPdfFromHtml(fullHtml, savePath);
    }

    toast.success("PDF saved", { id: toastId });
  } catch (err) {
    toast.error(`Export failed: ${getErrorMessage(err)}`, { id: toastId });
  }
}
