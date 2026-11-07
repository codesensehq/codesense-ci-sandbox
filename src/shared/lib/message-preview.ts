// SPDX-License-Identifier: MIT
/**
 * A message's one-line preview, as every inbox list shows it.
 *
 * The sender's `summary` wins when there is one: it is the line they chose for
 * exactly this. Otherwise the preview is derived from the body as the reader
 * would see it rendered, never as raw markup:
 *  - `html`: the document's `<title>` (or first heading), then its visible
 *    text, with scripts, styles and tags gone;
 *  - `markdown` (the default when no type is given): the text with its
 *    emphasis, code, heading, quote and list marks removed and links reduced
 *    to their words;
 *  - `plain`: the text as is.
 * Whitespace collapses to single spaces and the result is cut to `max`
 * characters with an ellipsis.
 */
export interface PreviewableMessage {
  text: string;
  contentType?: "plain" | "markdown" | "html" | null;
  summary?: string | null;
}

export const PREVIEW_MAX_CHARS = 160;

export function messagePreview(m: PreviewableMessage, max: number = PREVIEW_MAX_CHARS): string {
  const summary = collapse(m.summary ?? "");
  if (summary) return clip(summary, max);
  const type = m.contentType ?? "markdown";
  const text =
    type === "html" ? htmlToPlain(m.text) : type === "plain" ? m.text : markdownToPlain(m.text);
  return clip(collapse(text), max);
}

function collapse(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

/** Cut to `max` characters (code points, so an emoji is never split). */
function clip(s: string, max: number): string {
  const chars = Array.from(s);
  if (chars.length <= max) return s;
  return `${chars
    .slice(0, Math.max(0, max - 1))
    .join("")
    .trimEnd()}…`;
}

/** Inline HTML a markdown body may carry. Only real tag names: `Vec<String>`
 *  or `a <b and c> d` is text, not markup. */
const INLINE_HTML =
  /<\/?(?:a|abbr|b|br|code|del|details|div|em|h[1-6]|hr|i|img|ins|kbd|li|mark|ol|p|pre|s|small|span|strong|sub|summary|sup|table|tbody|td|th|thead|tr|u|ul)(?:\s[^<>]*)?\/?>/gi;

/** Markdown as the words a reader sees. */
export function markdownToPlain(md: string): string {
  return (
    md
      // Fenced code: keep the code, drop the fences (and their language tag).
      .replace(/^\s*(```|~~~)[^\n]*$/gm, "")
      // Images keep their alt text; links keep their words.
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      .replace(/\[([^\]]+)\]\[[^\]]*\]/g, "$1")
      .replace(/<(https?:\/\/[^>\s]+)>/g, "$1")
      // Inline HTML tags.
      .replace(INLINE_HTML, "")
      // Line-leading marks: headings, quotes, list bullets / numbers, rules, tables.
      .replace(/^\s{0,3}#{1,6}\s+/gm, "")
      .replace(/^\s{0,3}>\s?/gm, "")
      .replace(/^\s*(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?/gm, "")
      .replace(/^\s*(?:[-*_]\s*){3,}$/gm, "")
      .replace(/^\s*\|?(?:\s*:?-{3,}:?\s*\|)+\s*:?-{3,}:?\s*\|?\s*$/gm, "")
      .replace(/\s*\|\s*/g, " ")
      // Emphasis, strikethrough and inline code.
      .replace(/(\*\*|__)(?=\S)([\s\S]*?\S)\1/g, "$2")
      .replace(/(\*|_)(?=\S)([^*_\n]*?\S)\1(?![\w*])/g, "$2")
      .replace(/~~(?=\S)([\s\S]*?\S)~~/g, "$1")
      .replace(/`+([^`]*)`+/g, "$1")
      // Backslash escapes.
      .replace(/\\([\\`*_{}[\]()#+\-.!>|~])/g, "$1")
  );
}

/** An HTML document as a heading plus the words it shows. Parsed inert — no
 *  script runs, no resource loads. */
export function htmlToPlain(html: string): string {
  const doc = parseInert(html);
  if (!doc) return regexHtmlToPlain(html);
  doc
    .querySelectorAll("script, style, noscript, template, head > :not(title)")
    .forEach((el) => el.remove());
  const title = collapse(doc.querySelector("title")?.textContent ?? "");
  const heading = collapse(doc.querySelector("h1, h2")?.textContent ?? "");
  const lead = title || heading;
  doc.querySelector("title")?.remove();
  // Block boundaries become spaces, so "<p>a</p><p>b</p>" reads "a b".
  doc.body
    ?.querySelectorAll("p, div, li, h1, h2, h3, h4, h5, h6, br, tr, td, th, section, article")
    .forEach((el) => el.append(" "));
  let body = collapse(doc.body?.textContent ?? "");
  if (lead && body.startsWith(lead)) body = body.slice(lead.length).trim();
  return lead && body ? `${lead} — ${body}` : lead || body;
}

function parseInert(html: string): Document | null {
  if (typeof DOMParser === "undefined") return null;
  try {
    return new DOMParser().parseFromString(html, "text/html");
  } catch {
    return null;
  }
}

/** No DOM (a worker, a test without one): the same reading, by pattern. */
function regexHtmlToPlain(html: string): string {
  const title = /<title[^>]*>([\s\S]*?)<\/title>/i.exec(html)?.[1] ?? "";
  const body = html
    .replace(/<(script|style|noscript|template|head)\b[\s\S]*?<\/\1>/gi, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'");
  const lead = collapse(title);
  const text = collapse(body);
  return lead && text ? `${lead} — ${text}` : lead || text;
}
