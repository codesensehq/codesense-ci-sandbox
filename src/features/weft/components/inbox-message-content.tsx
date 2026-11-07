// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import createDOMPurify, { type DOMPurify as DOMPurifyType } from "dompurify";
import ReactMarkdown, { type Components } from "react-markdown";

import { cn } from "@/lib/utils";
import { VIEWER_REHYPE_PLUGINS, VIEWER_REMARK_PLUGINS } from "@/shared/lib/markdown-render-plugins";
import type { InboxTeam, MessageContentType } from "@/tauri/commands";

import { InboxSandboxedHtml } from "../lib/inbox-sandbox";

import { InboxMessageDashboard } from "./inbox-message-dashboard";

/**
 * Render one message body. Three formats are supported; `markdown` is the
 * default (see `DESIGN-inbox-ux.md` §3).
 *
 * - `plain`     — whitespace-preserved text, no formatting.
 * - `markdown`  — GFM via the shared viewer plugin set (tables, strikethrough,
 *                 task lists, alerts, highlighted code fences). The same
 *                 renderer the Markdown viewer uses, so a message quoting a
 *                 code block looks exactly like a file quoting the same block.
 * - `html`      — a message to the human (`live` given) is a live dashboard:
 *                 the same frame, `window.codesense` bridge, pushed events and
 *                 action gate an HTML dashboard file gets, identified by the
 *                 message. Without `live` (a message between agents) it is
 *                 strictly sanitized with DOMPurify and rendered through the
 *                 Shadow-DOM sandbox perimeter (SW-IB1-C1) so wide `<pre>` /
 *                 `<table>` / `<img>` can't blow out of the message cell.
 */
export function InboxMessageContent({
  text,
  contentType,
  className,
  live,
}: {
  text: string;
  contentType?: MessageContentType | null;
  className?: string;
  /** The message's identity, for a message the human reads: an `html` body then
   *  renders as a live dashboard. */
  live?: { team: InboxTeam; messageId: number; sender: string } | null;
}): React.JSX.Element {
  const resolved: MessageContentType = contentType ?? "markdown";
  if (resolved === "plain") {
    return (
      <p
        data-testid="inbox-message-body"
        data-content-type="plain"
        className={cn(
          // `inbox-html` applies the `overflow-wrap: anywhere; word-break: normal`
          // root rule from inbox-html.css (SW-IB1-A1). PR #1937's `min-w-0
          // break-words` are preserved for Tailwind-utility consumers that
          // resolve before the scoped stylesheet.
          "inbox-html min-w-0 whitespace-pre-wrap break-words text-foreground",
          className,
        )}
      >
        {text}
      </p>
    );
  }
  if (resolved === "html") {
    if (live) {
      return (
        <InboxMessageDashboard
          team={live.team}
          messageId={live.messageId}
          sender={live.sender}
          className={className}
        />
      );
    }
    return <SanitizedHtml html={text} className={className} />;
  }
  return <MarkdownBody text={text} className={className} />;
}

/** A message body shows a picture inline only when the picture is inside it: an allowed `data:`
 *  image (`ALLOWED_DATA_IMAGE_URI`). Any other source becomes a link, which opens in the browser:
 *  loading it on show would tell its host who read the message. An allowlist, because the
 *  browser's URL parser forgives schemes a pattern for `https:` misses (`ht\ttps:`, a leading
 *  control character, `\\host/x.png`). */
function isInlineImageSource(src: string): boolean {
  return ALLOWED_DATA_IMAGE_URI.test(src.trim());
}

// A message is not a document: tables scroll in their own box instead of squeezing their
// columns to a character each inside a narrow bubble.
const MARKDOWN_COMPONENTS: Components = {
  table: ({ node: _node, ...props }) => (
    <div className="inbox-table-scroll">
      <table {...props} />
    </div>
  ),
  img: ({ node: _node, src, alt, ...props }) => {
    const source = typeof src === "string" ? src : "";
    if (isInlineImageSource(source)) return <img src={source} alt={alt} {...props} />;
    const name = alt?.trim() || source.trim();
    return source.trim() ? <a href={source}>{name}</a> : <span>{name}</span>;
  },
};

function MarkdownBody({
  text,
  className,
}: {
  text: string;
  className?: string;
}): React.JSX.Element {
  return (
    <div
      data-testid="inbox-message-body"
      data-content-type="markdown"
      className={cn(
        // `inbox-md` sets message-scale type (inbox-html.css): headings as bold body text,
        // no theme accents, wrapped tables and code. Deliberately not `prose`, whose
        // document scale and theme colours turn a heading into a purple banner.
        "inbox-html inbox-md min-w-0 max-w-none text-foreground",
        className,
      )}
    >
      <ReactMarkdown
        remarkPlugins={VIEWER_REMARK_PLUGINS}
        rehypePlugins={VIEWER_REHYPE_PLUGINS}
        components={MARKDOWN_COMPONENTS}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}

/** DOMPurify allowlist for inbox HTML. Explicit — a change here is a security
 *  review surface. `#text` is required because `ALLOWED_TAGS` REPLACES the
 *  default list and the default list is what admits text nodes. */
const HTML_ALLOWED_TAGS = [
  "#text",
  "a",
  "abbr",
  "b",
  "blockquote",
  "br",
  "code",
  "del",
  "div",
  "em",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "hr",
  "i",
  "img",
  "ins",
  "kbd",
  "li",
  "mark",
  "ol",
  "p",
  "pre",
  "q",
  "s",
  "samp",
  "small",
  "span",
  "strong",
  "sub",
  "sup",
  "table",
  "tbody",
  "td",
  "tfoot",
  "th",
  "thead",
  "tr",
  "u",
  "ul",
];

const HTML_ALLOWED_ATTR = [
  "href",
  "title",
  "lang",
  "dir",
  "class",
  "colspan",
  "rowspan",
  "alt",
  "src",
];

/**
 * URL schemes allowed by the sanitizer.
 *
 * **Data-URI policy** (SW-IB1-C1, Bug #3 fix):
 *   `data:image/{png,jpeg,jpg,gif,webp}` — allowed, for inline tiny icons
 *     (DESIGN § components.human-inbox.html-containment.stripped).
 *   `data:image/svg+xml` — NOT allowed in v0.0.19. SVG `data:` URIs can
 *     carry inline `<script>`; the sanitizer does not currently re-sanitize
 *     the embedded SVG payload, so we treat svg+xml as stripped. A follow-up
 *     may re-enable it behind a nested SVG-content sanitizer.
 *   Everything else — `data:text/html`, `data:application/*`, `data:text/*`,
 *   `javascript:`, `vbscript:`, `file:` — is stripped from any attribute
 *   (`src`, `href`, `srcset`, inline `style url(...)`).
 */
const INBOX_URL_REGEXP = /^(?:https?:|mailto:|#|data:image\/(?:png|jpe?g|gif|webp)[;,])/i;

/**
 * Wrap every `<table>` in `<div class="inbox-table-scroll">...</div>` so wide
 * tables scroll horizontally **inside the wrapper** instead of blowing out of
 * the message cell. Both render paths (Shadow DOM primary and `.inbox-html`
 * fallback) rely on this wrapper for horizontal-scroll containment to apply.
 *
 * Operates on an already-sanitized HTML fragment — tables mentioned here are
 * guaranteed to be real `<table>` DOM nodes, not text/script masquerade.
 */
function wrapTablesForScroll(html: string): string {
  if (typeof document === "undefined" || !html.includes("<table")) {
    return html;
  }
  const container = document.createElement("div");
  container.innerHTML = html;
  const tables = container.querySelectorAll("table");
  tables.forEach((table) => {
    // Idempotent: skip if already inside an .inbox-table-scroll wrapper.
    if (table.parentElement?.classList?.contains("inbox-table-scroll")) return;
    const wrap = document.createElement("div");
    wrap.className = "inbox-table-scroll";
    table.replaceWith(wrap);
    wrap.appendChild(table);
  });
  return container.innerHTML;
}

function SanitizedHtml({
  html,
  className,
}: {
  html: string;
  className?: string;
}): React.JSX.Element {
  // Sanitize first (strip scripts, iframes, bad URIs including non-image
  // `data:`), THEN wrap <table> nodes for scroll. Order matters: wrapping
  // after sanitize ensures we only wrap tables that survived the trust
  // boundary.
  const safe = useMemo(() => wrapTablesForScroll(sanitizeInboxHtml(html)), [html]);

  // Primary path: Shadow-DOM sandbox perimeter (SW-IB1-A2). The caller
  // (this function) owns sanitization; the sandbox is pure CSS isolation.
  // Fallback path: when the runtime cannot host a shadow root (SSR,
  // pre-hydration, or a policy-blocked embedded Chromium), the
  // `.inbox-html` scoped stylesheet (SW-IB1-A1) carries the same five
  // rules on the light DOM.
  if (canUseShadowSandbox()) {
    return (
      <div
        data-testid="inbox-message-body"
        data-content-type="html"
        data-render-path="shadow"
        className={cn("min-w-0 max-w-none", className)}
      >
        <InboxSandboxedHtml html={safe} />
      </div>
    );
  }

  return (
    <section
      data-testid="inbox-message-body"
      data-content-type="html"
      data-render-path="fallback"
      className={cn(
        // `inbox-html` applies the five sandbox rules via inbox-html.css;
        // `min-w-0` keeps the body shrinkable inside the inbox's grid cells.
        "inbox-html inbox-md min-w-0 max-w-none text-foreground",
        className,
      )}
      // Author whitespace kept, as the shadow path's `:host` keeps it: `inbox-md` (markdown's
      // `white-space: normal`) would otherwise collapse an HTML body's newlines here only.
      style={{ whiteSpace: "pre-wrap" }}
      dangerouslySetInnerHTML={{ __html: safe }}
    />
  );
}

/** Shadow-DOM primary path is used when the host runtime supports
 *  `Element.prototype.attachShadow`. We do NOT gate on constructable
 *  stylesheets here — `InboxSandboxedHtml` has its own `<style>`-element
 *  fallback inside the shadow root when `adoptedStyleSheets` is absent. */
function canUseShadowSandbox(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof Element !== "undefined" &&
    typeof Element.prototype.attachShadow === "function"
  );
}

function getPurify(): DOMPurifyType {
  const w = typeof window !== "undefined" ? window : undefined;
  // DOMPurify's WindowLike type is wider than lib.dom's Window — the cast is
  // the documented init pattern for jsdom-driven test environments.
  return w
    ? createDOMPurify(w as unknown as Parameters<typeof createDOMPurify>[0])
    : createDOMPurify();
}

/**
 * Pattern for an ALLOWED `data:` image URI. Mirrors the `data:image/*` arm
 * of `INBOX_URL_REGEXP`, but used directly from the attribute hook below.
 * Keep this in lock-step with that regex.
 */
const ALLOWED_DATA_IMAGE_URI = /^data:image\/(?:png|jpe?g|gif|webp)[;,]/i;

/** URL-like attributes the hook polices for `data:` URIs. Scoped narrowly
 *  so a benign `title="data:something"` tooltip is not punished. */
const URL_ATTRS_FOR_DATA_CHECK = new Set([
  "href",
  "src",
  "srcset",
  "xlink:href",
  "action",
  "formaction",
  "poster",
  "background",
  "data",
]);

/** Export for tests + for anywhere else inbox HTML needs sanitizing. The
 *  config is strict — scripts/iframes/forms blocked, event handlers stripped,
 *  dangerous URL schemes rejected. Only `data:image/{png,jpeg,gif,webp}` is
 *  allowed among `data:` URIs; see `INBOX_URL_REGEXP` above.
 *
 *  Why the hook: DOMPurify auto-allows `data:` URIs on `<img|audio|video|
 *  source|image|track>` via its DEFAULT_DATA_URI_TAGS allowlist, BEFORE our
 *  `ALLOWED_URI_REGEXP` is consulted. So `<img src="data:text/plain,...">`
 *  passes the default check even though it is not an image. The hook below
 *  rejects any `data:` URI on a URL-like attribute that is not an
 *  `image/{png,jpeg,gif,webp}` — making our policy the authoritative one. */
export function sanitizeInboxHtml(html: string): string {
  const purify = getPurify();
  if (!purify.isSupported) {
    return "";
  }
  purify.removeAllHooks();
  purify.addHook("uponSanitizeAttribute", (_node, data) => {
    const name = typeof data.attrName === "string" ? data.attrName.toLowerCase() : "";
    if (!URL_ATTRS_FOR_DATA_CHECK.has(name)) return;
    const value = typeof data.attrValue === "string" ? data.attrValue : "";
    if (/^\s*data:/i.test(value) && !ALLOWED_DATA_IMAGE_URI.test(value.trim())) {
      data.keepAttr = false;
      data.attrValue = "";
    }
  });
  try {
    const out = purify.sanitize(html, {
      ALLOWED_TAGS: HTML_ALLOWED_TAGS,
      ALLOWED_ATTR: HTML_ALLOWED_ATTR,
      ALLOWED_URI_REGEXP: INBOX_URL_REGEXP,
      FORBID_TAGS: ["script", "style", "iframe", "object", "embed", "form", "input", "textarea"],
      FORBID_ATTR: ["style", "srcset"],
      KEEP_CONTENT: false,
    });
    return typeof out === "string" ? linkRemoteImages(out) : "";
  } finally {
    purify.removeAllHooks();
  }
}

/** Replaces each `<img>` in already-sanitized HTML whose source is not an inline image
 *  (`isInlineImageSource`) with a link to that source, or with its alt text when it has none.
 *  It parses in an inert document: an `<img>` parsed into the live one starts loading even
 *  while detached. */
function linkRemoteImages(html: string): string {
  if (typeof document === "undefined" || !/<img/i.test(html)) {
    return html;
  }
  const inert = document.implementation.createHTMLDocument("");
  const container = inert.createElement("div");
  container.innerHTML = html;
  container.querySelectorAll("img").forEach((image) => {
    const src = image.getAttribute("src") ?? "";
    if (isInlineImageSource(src)) return;
    const name = image.getAttribute("alt")?.trim() || src.trim();
    if (!src.trim()) {
      image.replaceWith(inert.createTextNode(name));
      return;
    }
    const link = inert.createElement("a");
    link.setAttribute("href", src.trim());
    link.textContent = name;
    image.replaceWith(link);
  });
  return container.innerHTML;
}
