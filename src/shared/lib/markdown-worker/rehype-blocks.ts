// SPDX-License-Identifier: MIT
import type { Root as HastRoot, Element, Nodes, RootContent } from "hast";
import { unified } from "unified";
import rehypeStringify from "rehype-stringify";
import type { Block } from "./types";
import { resolveViewerLinkPath } from "../viewer-file-type";

const stringifier = unified().use(rehypeStringify);

function stringifyNode(node: RootContent): string {
  return stringifier.stringify({ type: "root", children: [node] } as HastRoot);
}

function extractText(el: Element): string {
  const parts: string[] = [];
  for (const child of el.children) {
    if (child.type === "text") parts.push(child.value);
    else if (child.type === "element") parts.push(extractText(child));
  }
  return parts.join("");
}

function codeLang(codeEl: Element): string | null {
  // hast canonically emits className as a string[], but some plugins
  // (notably anything that survives `rehype-raw`) normalize it to a
  // space-separated string. Accept both — returning null for either
  // silently regresses mermaid fences to inert code blocks.
  const raw = codeEl.properties?.className;
  const classes: string[] = Array.isArray(raw)
    ? raw.filter((c): c is string => typeof c === "string")
    : typeof raw === "string"
      ? raw.split(/\s+/).filter(Boolean)
      : [];
  const langClass = classes.find((c) => c.startsWith("language-"));
  return langClass ? langClass.slice("language-".length) : null;
}

/**
 * True iff this element is a `<pre>` wrapping a `<code>` — the standard
 * fenced-code-block shape remark-rehype produces.
 */
function isCodeFencePre(el: Element): Element | null {
  if (el.tagName !== "pre") return null;
  const codeEl = el.children.find(
    (c): c is Element => c.type === "element" && c.tagName === "code",
  );
  return codeEl ?? null;
}

/**
 * Wrap a code `<pre>` in-place with `<div class="code-wrap"><pre/><button
 * class="copy-btn" aria-label="Copy code"></button></div>`. Button has
 * no child text — `.copy-btn::after { content: "Copy" }` in CSS provides
 * the visible label, `.copied::after { content: "Copied" }` the success
 * state. Click handling happens via delegation on the scroll container;
 * no React island, no createRoot, no mount cost per code fence.
 */
function wrapCodeInPlace(pre: Element): Element {
  return {
    type: "element",
    tagName: "div",
    properties: { className: ["code-wrap"] },
    children: [
      pre,
      {
        type: "element",
        tagName: "button",
        properties: {
          type: "button",
          className: ["copy-btn"],
          "aria-label": "Copy code",
        },
        children: [],
      },
    ],
  };
}

/** Code-fence languages the worker extracts as React islands. Each one needs
 *  work that can't live as inert DOM: mermaid/d2 render async (→ WASM);
 *  SW-DX-5's `csquery` runs a live query + subscribes to the status stream.
 *  Adding a new island kind is a two-step: extend this union here, and add the
 *  matching component in markdown-viewer.
 */
type IslandKind = "mermaid" | "d2" | "csquery";

function diagramPlaceholder(id: number): Element {
  return {
    type: "element",
    tagName: "span",
    properties: { "data-island-id": String(id) },
    children: [],
  };
}

/**
 * Deep pre-order walk. For each <pre><code>:
 *   - island fence (mermaid, d2, csquery) → extract into Block + replace with
 *     placeholder span
 *   - any other language → wrap in-place with code-wrap + copy-btn markup
 *
 * Code fences stay in the inert DOM (visible, searchable, selectable) with
 * a sibling copy button that the container's delegated onClick handles.
 * Island fences need main-thread React work (async SVG render, or a live
 * query subscription), so they keep the placeholder + createRoot pattern.
 */
function walkAndTransform(
  parent: { children: RootContent[] } | Element,
  out: Block[],
  nextId: { value: number },
): void {
  for (let i = 0; i < parent.children.length; i++) {
    const node = parent.children[i] as RootContent;
    if (node.type !== "element") continue;
    const el = node;

    const codeEl = isCodeFencePre(el);
    if (codeEl) {
      const lang = codeLang(codeEl);
      const islandKind: IslandKind | null =
        lang === "mermaid"
          ? "mermaid"
          : lang === "d2"
            ? "d2"
            : lang === "csquery"
              ? "csquery"
              : null;
      if (islandKind) {
        const code = extractText(codeEl);
        // Empty / whitespace-only island fences have nothing to render (a
        // mermaid/d2 renderer would throw "No diagram type detected"; a
        // `csquery` block would mount an island with no query). Fall through
        // to the inline code-wrap path so the empty fence renders as a
        // harmless, copy-able empty code block instead.
        if (code.trim() === "") {
          parent.children[i] = wrapCodeInPlace(el) as unknown as RootContent;
        } else {
          const id = nextId.value++;
          out.push({ id, kind: islandKind, code });
          parent.children[i] = diagramPlaceholder(id) as unknown as RootContent;
        }
      } else {
        parent.children[i] = wrapCodeInPlace(el) as unknown as RootContent;
      }
      continue;
    }

    if ("children" in el && Array.isArray(el.children)) {
      walkAndTransform(el as Element, out, nextId);
    }
  }
}

/**
 * Classify each top-level root child. Code fences (via walkAndTransform,
 * recursively) are wrapped in-place; mermaid fences extract into Blocks
 * + leave placeholders. The outer loop then stringifies each
 * (now-possibly-mutated) top-level child as one `{kind: "html"}` or
 * `{kind: "table"}` block.
 */
export function splitHastToBlocks(root: HastRoot): Block[] {
  const blocks: Block[] = [];
  const nextId = { value: 0 };

  // First pass: mutate the tree — wrap code fences, extract mermaid
  // fences with placeholders. Emits mermaid Blocks along the way.
  walkAndTransform(root, blocks, nextId);

  // Second pass: stringify each top-level child as its own block. This
  // captures whatever wrapping the first pass did.
  for (const node of root.children) {
    if (node.type === "text") {
      if (node.value.trim() === "") continue;
      const id = nextId.value++;
      blocks.push({ id, kind: "html", html: stringifyNode(node) });
      continue;
    }

    if (node.type !== "element") {
      const id = nextId.value++;
      blocks.push({ id, kind: "html", html: stringifyNode(node) });
      continue;
    }

    const el = node;
    const id = nextId.value++;
    if (el.tagName === "table") {
      blocks.push({ id, kind: "table", html: stringifyNode(el) });
    } else {
      blocks.push({ id, kind: "html", html: stringifyNode(el) });
    }
  }

  return blocks;
}

/**
 * In-place rewrite of internal `<a href="*.md">` links. Mirrors the
 * path-resolution the 32.4 main-thread `<a>` override did, so
 * `data-internal-href` matches what the jsx rollback path would produce.
 *
 * - http(s) / anchor-only / non-.md hrefs left untouched
 * - relative / absolute / parent-relative .md hrefs normalized to an
 *   absolute path and written to `data-internal-href` (href itself is
 *   preserved so right-click / copy-link still works sensibly)
 */
export function rewriteInternalLinks(
  root: HastRoot,
  owningFilePath: string,
  teamContentPath: string,
): void {
  walk(root, (node) => {
    if (node.type !== "element" || node.tagName !== "a") return;
    const props = (node.properties ??= {});
    const href = props.href;
    if (typeof href !== "string") return;

    // Ignore any href that isn't a bare relative/absolute path:
    //   - `mailto:`, `javascript:`, `data:`, `tel:`, `file:`, `http(s):`
    //     and friends (any RFC-3986 scheme prefix)
    //   - protocol-relative `//cdn.example.com/...`
    //   - anchor-only `#heading`
    if (/^(\/\/|[a-z][a-z0-9+.-]*:)/i.test(href)) return;
    if (href.startsWith("#")) return;

    const resolved = resolveViewerLinkPath(href, owningFilePath, teamContentPath);
    if (!resolved) return;

    (props as Record<string, unknown>)["data-internal-href"] = resolved;
  });
}

function walk(node: Nodes, visit: (n: Nodes) => void): void {
  visit(node);
  if ("children" in node && Array.isArray(node.children)) {
    for (const child of node.children) walk(child as Nodes, visit);
  }
}
