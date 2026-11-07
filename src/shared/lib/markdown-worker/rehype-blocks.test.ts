// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import remarkFrontmatter from "remark-frontmatter";
import { remarkAlert } from "remark-github-blockquote-alert";
import remarkRehype from "remark-rehype";
import rehypeSlug from "rehype-slug";
import rehypeHighlight from "rehype-highlight";
import type { Root as HastRoot, Nodes } from "hast";
import { splitHastToBlocks, rewriteInternalLinks } from "./rehype-blocks";

const processor = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkFrontmatter)
  .use(remarkAlert)
  .use(remarkRehype)
  .use(rehypeSlug)
  .use(rehypeHighlight);

function parse(md: string): HastRoot {
  const mdast = processor.parse(md);
  return processor.runSync(mdast) as HastRoot;
}

describe("splitHastToBlocks — top-level classification", () => {
  it("top-level code fence stays inline in the html block, wrapped with code-wrap + copy-btn", () => {
    const hast = parse("```ts\nconst x = 1;\n```\n");
    const blocks = splitHastToBlocks(hast);
    expect(blocks).toHaveLength(1);
    const b = blocks[0];
    expect(b.kind).toBe("html");
    if (b.kind !== "html") throw new Error("narrow");
    expect(b.html).toContain('<div class="code-wrap">');
    expect(b.html).toContain('<button type="button" class="copy-btn"');
    expect(b.html).toContain('aria-label="Copy code"');
    // Still-hljs-highlighted <code> inside <pre>.
    expect(b.html).toMatch(/<pre><code class="[^"]*language-ts[^"]*"/);
    expect(b.html).toMatch(/<span class="hljs-/);
  });

  it("top-level mermaid fence becomes a mermaid Block with a data-island-id placeholder left in the html", () => {
    const hast = parse("```mermaid\nflowchart LR\n  A --> B\n```\n");
    const blocks = splitHastToBlocks(hast);
    const mermaidBlocks = blocks.filter((b) => b.kind === "mermaid");
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    expect(mermaidBlocks).toHaveLength(1);
    if (mermaidBlocks[0].kind !== "mermaid") throw new Error("narrow");
    expect(mermaidBlocks[0].code.trim()).toBe("flowchart LR\n  A --> B");
    expect(htmlBlocks).toHaveLength(1);
    if (htmlBlocks[0].kind !== "html") throw new Error("narrow");
    expect(htmlBlocks[0].html).toMatch(/<span data-island-id="\d+"><\/span>/);
  });

  it("top-level csquery fence becomes a csquery Block with a data-island-id placeholder (SW-DX-5)", () => {
    const hast = parse("```csquery\ntag: type=epic\nstatus: done\n```\n");
    const blocks = splitHastToBlocks(hast);
    const csqueryBlocks = blocks.filter((b) => b.kind === "csquery");
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    expect(csqueryBlocks).toHaveLength(1);
    if (csqueryBlocks[0].kind !== "csquery") throw new Error("narrow");
    expect(csqueryBlocks[0].code.trim()).toBe("tag: type=epic\nstatus: done");
    expect(htmlBlocks).toHaveLength(1);
    if (htmlBlocks[0].kind !== "html") throw new Error("narrow");
    expect(htmlBlocks[0].html).toMatch(/<span data-island-id="\d+"><\/span>/);
  });

  it("empty csquery fence degrades to an inert copy-able code block, not an island (SW-DX-5 AC-9)", () => {
    const blocks = splitHastToBlocks(parse("```csquery\n \n```\n"));
    expect(blocks.filter((b) => b.kind === "csquery")).toHaveLength(0);
    expect(blocks).toHaveLength(1);
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toContain('<div class="code-wrap">');
    expect(blocks[0].html).toContain('<button type="button" class="copy-btn"');
  });

  it("top-level paragraph becomes a single html Block", () => {
    const hast = parse("just a paragraph.\n");
    const blocks = splitHastToBlocks(hast);
    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe("html");
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toMatch(/^<p>/);
  });

  it("top-level table becomes a table Block", () => {
    const md = "| a | b |\n| - | - |\n| 1 | 2 |\n";
    const hast = parse(md);
    const blocks = splitHastToBlocks(hast);
    expect(blocks.filter((b) => b.kind === "table")).toHaveLength(1);
  });

  it("each top-level child maps to its own Block", () => {
    const md = `# heading

para 1

\`\`\`ts
a
\`\`\`

para 2
`;
    const blocks = splitHastToBlocks(parse(md));
    // 4 top-level children: heading, para 1, code fence (wrapped in-place),
    // para 2 — all emit as html.
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    expect(htmlBlocks).toHaveLength(4);
    // The third html block carries the wrapped code fence.
    if (htmlBlocks[2].kind !== "html") throw new Error("narrow");
    expect(htmlBlocks[2].html).toContain('class="code-wrap"');
  });

  it("assigns sequential numeric ids so React can key by b.id", () => {
    const blocks = splitHastToBlocks(parse("# a\n\n# b\n\n# c\n"));
    expect(blocks.map((b) => b.id)).toEqual([0, 1, 2]);
  });
});

describe("splitHastToBlocks — deep-walk transforms (spike → production fix)", () => {
  it("code fence nested inside a list item stays inline, wrapped with code-wrap + copy-btn", () => {
    const md = `- item 1
  \`\`\`ts
  nestedFn();
  \`\`\`
- item 2
`;
    const blocks = splitHastToBlocks(parse(md));
    expect(blocks).toHaveLength(1); // one list → one html block
    const b = blocks[0];
    if (b.kind !== "html") throw new Error("narrow");
    // The nested code fence was wrapped in-place, not extracted.
    expect(b.html).toContain('<div class="code-wrap">');
    expect(b.html).toContain('<button type="button" class="copy-btn"');
    // Code content is still in the DOM — searchable via textContent even
    // though rehype-highlight wraps tokens in <span class="hljs-*"> that
    // break literal substring matches on the full code text.
    expect(b.html).toMatch(/nestedFn/);
    // No data-island-id placeholder (code is not an island anymore).
    expect(b.html).not.toMatch(/data-island-id/);
  });

  it("mermaid fence nested inside a blockquote extracts as mermaid Block + leaves placeholder span", () => {
    const md = `> quote start
>
> \`\`\`mermaid
> flowchart TD
>   X --> Y
> \`\`\`
>
> quote end
`;
    const blocks = splitHastToBlocks(parse(md));
    expect(blocks.filter((b) => b.kind === "mermaid")).toHaveLength(1);
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    if (htmlBlocks[0].kind !== "html") throw new Error("narrow");
    expect(htmlBlocks[0].html).toMatch(/<blockquote>/);
    expect(htmlBlocks[0].html).toMatch(/data-island-id=/);
  });

  it("wraps code fences nested at arbitrary depth (no extraction)", () => {
    const md = `- outer
  1. inner ordered
     - deepest
       \`\`\`js
       veryDeep();
       \`\`\`
`;
    const blocks = splitHastToBlocks(parse(md));
    expect(blocks.filter((b) => b.kind === "mermaid")).toHaveLength(0);
    expect(blocks).toHaveLength(1);
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toContain('<div class="code-wrap">');
    expect(blocks[0].html).toMatch(/veryDeep/);
  });

  it("wraps both top-level and nested code fences in one splitter call", () => {
    const md = `\`\`\`ts
topLevel();
\`\`\`

- list item
  \`\`\`py
  nested();
  \`\`\`
`;
    const blocks = splitHastToBlocks(parse(md));
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    expect(htmlBlocks).toHaveLength(2);
    const htmls = htmlBlocks
      .filter((b) => b.kind === "html")
      .map((b) => (b.kind === "html" ? b.html : ""));
    // Each html block contains one code-wrap.
    for (const html of htmls) expect(html).toContain('<div class="code-wrap">');
    // Code content preserved (loose match — rehype-highlight interleaves
    // <span> tokens that break literal substring matches on full lines).
    expect(htmls.some((h) => /topLevel/.test(h))).toBe(true);
    expect(htmls.some((h) => /nested/.test(h))).toBe(true);
  });

  it("detects language when className is a space-separated string, not only when it's a string[] (regression guard)", () => {
    // Most hast producers emit className as string[] but some
    // rehype-raw flows normalize it to a space-separated string. The
    // splitter must still route mermaid fences correctly in that case
    // — otherwise they silently render as inert code blocks.
    const fakeHast: HastRoot = {
      type: "root",
      children: [
        {
          type: "element",
          tagName: "pre",
          properties: {},
          children: [
            {
              type: "element",
              tagName: "code",
              // string, not string[] — the shape a rehype-raw pass can
              // produce after round-tripping through serialized HTML.
              properties: { className: "language-mermaid hljs" },
              children: [{ type: "text", value: "flowchart LR\n  A --> B" }],
            },
          ],
        },
      ],
    };
    const blocks = splitHastToBlocks(fakeHast);
    expect(blocks.filter((b) => b.kind === "mermaid")).toHaveLength(1);
  });

  it("empty / whitespace-only mermaid fence falls through to inline code-wrap (no mermaid.render error)", () => {
    // Mermaid's render throws 'No diagram type detected' on empty
    // source, which would surface as a red error card per fence. Code
    // review patch: treat empty fences as plain code blocks.
    const md = "```mermaid\n```\n\n```mermaid\n   \n```\n";
    const blocks = splitHastToBlocks(parse(md));
    expect(blocks.filter((b) => b.kind === "mermaid")).toHaveLength(0);
    for (const b of blocks) {
      if (b.kind !== "html") continue;
      expect(b.html).toContain('<div class="code-wrap">');
    }
  });

  it("mermaid placeholder ids in the emitted html match the corresponding mermaid Block ids", () => {
    const md = `- item with
  \`\`\`mermaid
  flowchart LR
    A --> B
  \`\`\`
`;
    const blocks = splitHastToBlocks(parse(md));
    const mermaidBlock = blocks.find((b) => b.kind === "mermaid")!;
    const htmlBlock = blocks.find((b) => b.kind === "html")!;
    if (htmlBlock.kind !== "html") throw new Error("narrow");
    expect(htmlBlock.html).toContain(`data-island-id="${mermaidBlock.id}"`);
  });
});

describe("rewriteInternalLinks", () => {
  const owning = "/repo/docs/readme.md";

  function runLink(href: string): string | null | undefined {
    const hast = parse(`[link](${href})\n`);
    rewriteInternalLinks(hast, owning, "");
    let found: string | null | undefined = null;
    walk(hast, (node) => {
      if (node.type === "element" && node.tagName === "a") {
        const props = node.properties as Record<string, unknown> | undefined;
        found = props?.["data-internal-href"] as string | undefined;
      }
    });
    return found;
  }

  it("relative .md link → resolved absolute path", () => {
    expect(runLink("./sibling.md")).toBe("/repo/docs/sibling.md");
  });

  it("sibling .md link without ./ → same resolution", () => {
    expect(runLink("sibling.md")).toBe("/repo/docs/sibling.md");
  });

  it("absolute /path.md under content/ resolves to repo root", () => {
    // PTS-2: anchor segment is `/content/` (was `/_bmad-output/`).
    const hast = parse("[link](/docs/x.md)\n");
    rewriteInternalLinks(hast, "/repo/content/impl/story.md", "");
    let found: string | undefined;
    walk(hast, (node) => {
      if (node.type === "element" && node.tagName === "a") {
        const props = node.properties as Record<string, unknown> | undefined;
        found = props?.["data-internal-href"] as string | undefined;
      }
    });
    expect(found).toBe("/repo/docs/x.md");
  });

  it("anchor-only link (#heading) is left alone", () => {
    expect(runLink("#foo")).toBeUndefined();
  });

  it("http(s) external link is left alone", () => {
    expect(runLink("https://example.com/x.md")).toBeUndefined();
    expect(runLink("http://example.com/x.md")).toBeUndefined();
  });

  it("non-.md relative link is left alone", () => {
    expect(runLink("./foo.txt")).toBeUndefined();
  });

  it("link with anchor fragment keeps resolution and drops the fragment in the data attr", () => {
    expect(runLink("./page.md#section")).toBe("/repo/docs/page.md");
  });

  it("parent-relative link resolves through ..", () => {
    const hast = parse("[link](../adjacent/file.md)\n");
    rewriteInternalLinks(hast, "/repo/docs/sub/a.md", "");
    let found: string | undefined;
    walk(hast, (node) => {
      if (node.type === "element" && node.tagName === "a") {
        const props = node.properties as Record<string, unknown> | undefined;
        found = props?.["data-internal-href"] as string | undefined;
      }
    });
    expect(found).toBe("/repo/docs/adjacent/file.md");
  });

  // Code-review patch: scheme-sniff + proper .md suffix match
  it("mailto:/tel:/javascript:/data: URIs are left alone even if their payload contains .md", () => {
    expect(runLink("mailto:alice@foo.md")).toBeUndefined();
    expect(runLink("javascript:openFoo.md()")).toBeUndefined();
    expect(runLink("data:text/markdown,hello.md")).toBeUndefined();
    expect(runLink("tel:+1555.md")).toBeUndefined();
    expect(runLink("file:///tmp/x.md")).toBeUndefined();
  });

  it("protocol-relative URL (//cdn.example.com/readme.md) is left alone", () => {
    expect(runLink("//cdn.example.com/readme.md")).toBeUndefined();
  });

  it(".markdown / .mdx / .md5 / .md.bak do NOT match (substring .md used to over-match)", () => {
    expect(runLink("./x.markdown")).toBeUndefined();
    expect(runLink("./x.mdx")).toBeUndefined();
    expect(runLink("./x.md5")).toBeUndefined();
    expect(runLink("./x.md.bak")).toBeUndefined();
  });

  // R.2: HTML link navigation
  it("relative .html link → resolved absolute path", () => {
    expect(runLink("./mockup.html")).toBe("/repo/docs/mockup.html");
  });

  it("relative .htm link → resolved absolute path", () => {
    expect(runLink("./page.htm")).toBe("/repo/docs/page.htm");
  });

  it(".html link with fragment keeps resolution", () => {
    expect(runLink("./dashboard.html#section")).toBe("/repo/docs/dashboard.html");
  });

  it(".HTML uppercase link → resolved", () => {
    expect(runLink("./REPORT.HTML")).toBe("/repo/docs/REPORT.HTML");
  });

  it(".html5 / .htmlx do NOT match", () => {
    expect(runLink("./x.html5")).toBeUndefined();
    expect(runLink("./x.htmlx")).toBeUndefined();
  });
});

describe("inline code rendering", () => {
  it("single-backtick inline code renders as <code>…</code> with no literal backticks", () => {
    const blocks = splitHastToBlocks(parse("A paragraph with `inline` code.\n"));
    expect(blocks).toHaveLength(1);
    if (blocks[0].kind !== "html") throw new Error("narrow");
    const html = blocks[0].html;
    expect(html).toMatch(/<code>inline<\/code>/);
    expect(html).not.toContain("`");
  });
});

describe("D2 diagram fences", () => {
  it("top-level `language-d2` fence becomes a d2 Block with a data-island-id placeholder left in the html", () => {
    const hast = parse("```d2\na -> b\n```\n");
    const blocks = splitHastToBlocks(hast);
    const d2Blocks = blocks.filter((b) => b.kind === "d2");
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    expect(d2Blocks).toHaveLength(1);
    if (d2Blocks[0].kind !== "d2") throw new Error("narrow");
    expect(d2Blocks[0].code.trim()).toBe("a -> b");
    expect(htmlBlocks).toHaveLength(1);
    if (htmlBlocks[0].kind !== "html") throw new Error("narrow");
    expect(htmlBlocks[0].html).toMatch(/<span data-island-id="\d+"><\/span>/);
  });

  it("nested d2 fence inside a blockquote extracts as d2 Block + placeholder", () => {
    const md = `> intro
>
> \`\`\`d2
> x -> y
> \`\`\`
`;
    const blocks = splitHastToBlocks(parse(md));
    expect(blocks.filter((b) => b.kind === "d2")).toHaveLength(1);
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    if (htmlBlocks[0].kind !== "html") throw new Error("narrow");
    expect(htmlBlocks[0].html).toMatch(/<blockquote>/);
    expect(htmlBlocks[0].html).toMatch(/data-island-id=/);
  });

  it("empty d2 fence falls through to inline code-wrap (no compile error card)", () => {
    const blocks = splitHastToBlocks(parse("```d2\n\n```\n"));
    expect(blocks.filter((b) => b.kind === "d2")).toHaveLength(0);
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toContain('class="code-wrap"');
  });

  it("id space is shared between d2 and mermaid blocks so placeholders stay unique", () => {
    const md = "```mermaid\nA --> B\n```\n\n```d2\nx -> y\n```\n";
    const blocks = splitHastToBlocks(parse(md));
    const ids = blocks.map((b) => b.id);
    // All 4 blocks (mermaid, placeholder-html, d2, placeholder-html) get
    // distinct ids; islands can match on id without collisions.
    expect(new Set(ids).size).toBe(blocks.length);
  });
});

describe("GitHub-flavored alerts (remark-github-blockquote-alert)", () => {
  it.each([
    ["NOTE", "note"],
    ["TIP", "tip"],
    ["IMPORTANT", "important"],
    ["WARNING", "warning"],
    ["CAUTION", "caution"],
  ])("converts `> [!%s]` into a <div class=markdown-alert-%s>", (tag, klass) => {
    const md = `> [!${tag}]\n> body text\n`;
    const blocks = splitHastToBlocks(parse(md));
    expect(blocks).toHaveLength(1);
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toContain(`markdown-alert-${klass}`);
    expect(blocks[0].html).toContain("markdown-alert-title");
    expect(blocks[0].html).toContain("body text");
  });

  it("leaves a plain blockquote (no `[!TYPE]` marker) unchanged", () => {
    const blocks = splitHastToBlocks(parse("> plain quote\n"));
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toMatch(/<blockquote>/);
    expect(blocks[0].html).not.toContain("markdown-alert");
  });
});

describe("rehype-slug — heading IDs for anchor navigation", () => {
  it("emits github-style kebab-case id on an h2", () => {
    const blocks = splitHastToBlocks(parse("## What Makes This Special\n"));
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toMatch(/<h2 id="what-makes-this-special">/);
  });

  it("emits id on h3 with punctuation stripped", () => {
    const blocks = splitHastToBlocks(parse("### Project Context (brownfield)\n"));
    if (blocks[0].kind !== "html") throw new Error("narrow");
    expect(blocks[0].html).toMatch(/<h3 id="project-context-brownfield">/);
  });

  it("disambiguates duplicate heading slugs with numeric suffix", () => {
    const blocks = splitHastToBlocks(
      parse("## Overview\n\nfirst section.\n\n## Overview\n\nsecond section.\n"),
    );
    const htmlBlocks = blocks.filter((b) => b.kind === "html");
    const combined = htmlBlocks.map((b) => (b.kind === "html" ? b.html : "")).join("\n");
    expect(combined).toMatch(/<h2 id="overview">/);
    expect(combined).toMatch(/<h2 id="overview-1">/);
  });
});

function walk(node: Nodes, visit: (n: Nodes) => void): void {
  visit(node);
  if ("children" in node && Array.isArray(node.children)) {
    for (const child of node.children) walk(child as Nodes, visit);
  }
}
