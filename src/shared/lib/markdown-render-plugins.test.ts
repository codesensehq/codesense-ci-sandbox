// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import remarkGfm from "remark-gfm";
import remarkFrontmatter from "remark-frontmatter";
import { remarkAlert } from "remark-github-blockquote-alert";
import rehypeHighlight from "rehype-highlight";

import { VIEWER_REMARK_PLUGINS, VIEWER_REHYPE_PLUGINS } from "./markdown-render-plugins";

// The DRY contract: the read-only MarkdownViewer sync path and the SW-VE-10
// live-preview pane MUST render the same markdown flavour. Pin the shared plugin
// sets so a divergence (a plugin added to one surface but not the other) fails
// here instead of silently drifting "what I edit" from "what it renders".
describe("markdown-render-plugins — shared react-markdown pipeline (SW-VE-10 DRY)", () => {
  it("remark set is gfm + frontmatter + alert, in order", () => {
    expect(VIEWER_REMARK_PLUGINS).toEqual([remarkGfm, remarkFrontmatter, remarkAlert]);
  });

  it("rehype set is highlight", () => {
    expect(VIEWER_REHYPE_PLUGINS).toEqual([rehypeHighlight]);
  });
});
