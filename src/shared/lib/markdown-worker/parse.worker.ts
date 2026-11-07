// SPDX-License-Identifier: MIT
/// <reference lib="webworker" />
import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import remarkFrontmatter from "remark-frontmatter";
import { remarkAlert } from "remark-github-blockquote-alert";
import remarkRehype from "remark-rehype";
import rehypeSlug from "rehype-slug";
import rehypeHighlight from "rehype-highlight";
import type { Root as HastRoot } from "hast";
import rehypeStringify from "rehype-stringify";
import type { ParseRequest, ParseResponse } from "./types";
import { splitHastToBlocks, rewriteInternalLinks } from "./rehype-blocks";
import { buildRedlineHast, surfaceFrontmatter } from "./redline";

/** Stringifies a full HAST root to an HTML string — used by export mode. */
const exportStringifier = unified().use(rehypeStringify);

const processor = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkFrontmatter)
  // Must run before remark-rehype: operates on mdast blockquote nodes,
  // attaches hProperties that survive the mdast→hast conversion.
  .use(remarkAlert)
  .use(remarkRehype)
  // rehype-slug must run on hast (after remark-rehype) and before
  // any walker that serializes heading output — adds github-style
  // kebab-case `id` attributes to headings so `#anchor` links resolve.
  .use(rehypeSlug)
  .use(rehypeHighlight);

self.addEventListener("message", (ev: MessageEvent<ParseRequest>) => {
  const { requestId, mode } = ev.data;
  try {
    if (ev.data.mode === "redline") {
      // SW-GS-3: parse both sides through the SAME pipeline, then block-align
      // into one annotated ins/del HAST root.
      const { before, after } = ev.data;
      const r0 = performance.timeOrigin + performance.now();
      // Surface frontmatter as a visible code block so a frontmatter-only
      // change (e.g. a story status transition) still shows in the redline.
      const beforeHast = processor.runSync(processor.parse(surfaceFrontmatter(before))) as HastRoot;
      const afterHast = processor.runSync(processor.parse(surfaceFrontmatter(after))) as HastRoot;
      const r1 = performance.timeOrigin + performance.now();
      const hast = buildRedlineHast(beforeHast, afterHast);
      const r2 = performance.timeOrigin + performance.now();
      const resp: ParseResponse = {
        requestId,
        ok: true,
        mode: "redline",
        hast,
        perf: {
          parseMs: r1 - r0,
          runMs: r2 - r1,
          postAt: performance.timeOrigin + performance.now(),
        },
      };
      self.postMessage(resp);
      return;
    }

    const text = ev.data.text;
    const t0 = performance.timeOrigin + performance.now();
    const mdast = processor.parse(text);
    const t1 = performance.timeOrigin + performance.now();
    const hast = processor.runSync(mdast) as HastRoot;
    const t2 = performance.timeOrigin + performance.now();

    if (mode === "hast") {
      const resp: ParseResponse = {
        requestId,
        ok: true,
        mode: "hast",
        hast,
        perf: {
          parseMs: t1 - t0,
          runMs: t2 - t1,
          postAt: performance.timeOrigin + performance.now(),
        },
      };
      self.postMessage(resp);
      return;
    }

    if (mode === "export") {
      // Stringify the full HAST root to HTML — no walkAndTransform, so
      // code fences stay as clean <pre><code> without copy-button wrappers.
      // Mermaid/d2 fences remain as <pre><code class="language-mermaid">.
      const html = exportStringifier.stringify(hast);
      const resp: ParseResponse = {
        requestId,
        ok: true,
        mode: "export",
        html,
        perf: {
          parseMs: t1 - t0,
          runMs: t2 - t1,
          postAt: performance.timeOrigin + performance.now(),
        },
      };
      self.postMessage(resp);
      return;
    }

    // mode === "blocks" — rewrite internal .md links to carry a resolved
    // data-internal-href, then deep-walk split into islands + html blocks.
    rewriteInternalLinks(hast, ev.data.filePath, ev.data.teamContentPath);
    const blocks = splitHastToBlocks(hast);
    const t3 = performance.timeOrigin + performance.now();

    const resp: ParseResponse = {
      requestId,
      ok: true,
      mode: "blocks",
      blocks,
      perf: {
        parseMs: t1 - t0,
        runMs: t2 - t1,
        splitMs: t3 - t2,
        postAt: performance.timeOrigin + performance.now(),
      },
    };
    self.postMessage(resp);
  } catch (e) {
    const resp: ParseResponse = {
      requestId,
      ok: false,
      error: e instanceof Error ? e.message : String(e),
    };
    self.postMessage(resp);
  }
});

export {};
