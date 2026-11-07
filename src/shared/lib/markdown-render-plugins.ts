// SPDX-License-Identifier: MIT
import type { PluggableList } from "unified";
import remarkGfm from "remark-gfm";
import remarkFrontmatter from "remark-frontmatter";
import { remarkAlert } from "remark-github-blockquote-alert";
import rehypeHighlight from "rehype-highlight";

// ---------------------------------------------------------------------------
// The single source of truth for the in-app `react-markdown` plugin pipeline,
// shared by every string→JSX markdown render surface:
//   - the read-only `MarkdownViewer` sync-render fallback
//     (`src/features/dashboard/components/markdown-viewer.tsx`), and
//   - the SW-VE-10 live-preview pane (`markdown-preview-pane.tsx`), which
//     renders the live editor buffer.
//
// Keeping the plugin set in one place means the rendered preview is byte-for-
// byte the same markdown flavour the read-only viewer shows (GFM tables /
// strikethrough / task lists, YAML frontmatter parsed-and-hidden, GitHub
// blockquote alerts, highlighted code fences) — no drift between "what I'm
// editing" and "what it will look like".
//
// NB: the heavy `MarkdownViewer` worker path (`markdown-worker/parse.worker.ts`)
// composes the equivalent `unified` pipeline directly for off-main-thread
// parsing of large SAVED files; that is a different toolchain (remark-rehype +
// rehype-slug + block splitting) and is intentionally NOT unified with this
// `react-markdown` config — a live, per-keystroke preview wants the light
// synchronous renderer, not the worker/morphdom/island machinery.
// ---------------------------------------------------------------------------

/** Remark plugins shared by the read-only viewer sync path and the live preview. */
export const VIEWER_REMARK_PLUGINS: PluggableList = [remarkGfm, remarkFrontmatter, remarkAlert];

/** Rehype plugins shared by the same surfaces — syntax-highlight code fences. */
export const VIEWER_REHYPE_PLUGINS: PluggableList = [rehypeHighlight];
