// SPDX-License-Identifier: MIT
// @vitest-environment jsdom
import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { render, screen } from "@/test-utils";

import { InboxMessageContent, sanitizeInboxHtml } from "./inbox-message-content";

/**
 * The sanitizer is the trust boundary for `contentType: "html"`. These tests
 * assert the lock, not the UI chrome.
 */
describe("sanitizeInboxHtml", () => {
  it("strips <script> tags and their contents", () => {
    const out = sanitizeInboxHtml("<p>hi</p><script>alert(1)</script>");
    expect(out).toContain("<p>hi</p>");
    expect(out).not.toContain("<script");
    expect(out).not.toContain("alert");
  });

  it("strips event-handler attributes", () => {
    const out = sanitizeInboxHtml('<a href="https://ok" onclick="alert(1)">x</a>');
    expect(out).toContain("href");
    expect(out).not.toContain("onclick");
    expect(out).not.toContain("alert");
  });

  it("rejects javascript: URLs on <a href>", () => {
    const out = sanitizeInboxHtml('<a href="javascript:alert(1)">x</a>');
    expect(out).not.toContain("javascript:");
    expect(out).not.toContain("alert");
  });

  it("drops <iframe>, <object>, <embed>, <form>, <input>", () => {
    const out = sanitizeInboxHtml(
      '<iframe src="https://evil"></iframe><object></object><embed /><form><input /></form>',
    );
    expect(out).not.toMatch(/<(iframe|object|embed|form|input)/i);
  });

  it("drops <style> and inline style= attributes", () => {
    const out = sanitizeInboxHtml('<p style="background:red">x</p><style>.a{}</style>');
    expect(out).not.toContain("<style");
    expect(out).not.toContain("style=");
  });

  it("keeps formatting allowlist (headings, lists, code, tables, blockquotes, strong, em)", () => {
    const out = sanitizeInboxHtml(
      "<h1>t</h1><ul><li><code>c</code></li></ul><p><strong>b</strong> <em>i</em></p><blockquote>q</blockquote><table><tr><td>cell</td></tr></table>",
    );
    expect(out).toContain("<h1>t</h1>");
    expect(out).toContain("<code>c</code>");
    expect(out).toContain("<strong>b</strong>");
    expect(out).toContain("<em>i</em>");
    expect(out).toContain("<blockquote>q</blockquote>");
    expect(out).toContain("<td>cell</td>");
  });

  // SW-IB1-C1 — data:-URI policy. `data:image/*` (png/jpeg/gif/webp) passes
  // through for inline tiny icons; everything else is stripped.
  describe("data: URI policy", () => {
    it("passes a data:image/png;base64 URI through on <img src>", () => {
      const png =
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
      const out = sanitizeInboxHtml(`<img src="${png}" alt="dot" />`);
      expect(out).toContain("<img");
      expect(out).toContain(png);
    });

    it("passes data:image/jpeg and data:image/gif and data:image/webp through", () => {
      for (const mime of ["image/jpeg", "image/jpg", "image/gif", "image/webp"]) {
        const uri = `data:${mime};base64,AAAA`;
        const out = sanitizeInboxHtml(`<img src="${uri}" alt="x" />`);
        expect(out, `allow ${mime}`).toContain(uri);
      }
    });

    it("strips data:text/html on <a href>", () => {
      const out = sanitizeInboxHtml('<a href="data:text/html,<script>alert(1)</script>">x</a>');
      expect(out).not.toContain("data:text/html");
      expect(out).not.toContain("<script");
    });

    it("strips data:application/javascript on <a href>", () => {
      const out = sanitizeInboxHtml('<a href="data:application/javascript,alert(1)">x</a>');
      expect(out).not.toContain("data:application");
      expect(out).not.toContain("alert");
    });

    it("strips data:image/svg+xml (SVG payloads can carry inline <script>)", () => {
      const out = sanitizeInboxHtml(
        '<img src="data:image/svg+xml;utf8,<svg xmlns=\'http://www.w3.org/2000/svg\'><script>alert(1)</script></svg>" alt="x" />',
      );
      expect(out).not.toContain("data:image/svg");
      expect(out).not.toContain("<script");
    });

    it("strips a data:text/plain URI on <img src>", () => {
      const out = sanitizeInboxHtml('<img src="data:text/plain,hello" alt="x" />');
      expect(out).not.toContain("data:text/plain");
    });
  });

  describe("remote images", () => {
    it("turns a remote <img> into a link to it, named by its alt", () => {
      const out = sanitizeInboxHtml('<p><img src="https://host.example/a.png" alt="chart"></p>');
      expect(out).not.toContain("<img");
      expect(out).toContain('<a href="https://host.example/a.png">chart</a>');
    });

    it("names the link by its address when the image has no alt", () => {
      const out = sanitizeInboxHtml('<img src="http://host.example/a.png">');
      expect(out).toContain('<a href="http://host.example/a.png">http://host.example/a.png</a>');
    });

    it("keeps a data: image", () => {
      const out = sanitizeInboxHtml('<img src="data:image/png;base64,AAAA" alt="dot">');
      expect(out).toContain("<img");
    });

    // The browser drops a tab or newline anywhere in a URL and C0 controls before it, so each
    // of these loads https://host.example/a.png. Only an inline data: image stays an <img>.
    it.each([
      ["a tab in the scheme", '<img src="ht&#9;tps://host.example/a.png" alt="chart">'],
      ["a newline in the scheme", '<img src="ht&#10;tps://host.example/a.png" alt="chart">'],
      ["a leading control character", '<img src="&#1;https://host.example/a.png" alt="chart">'],
      ["an entity-encoded scheme", '<img src="&#104;ttps://host.example/a.png" alt="chart">'],
    ])("links an image with %s instead of loading it", (_form, html) => {
      const out = sanitizeInboxHtml(html);
      expect(out).not.toContain("<img");
      expect(out).toMatch(/<a href="[^"]*">chart<\/a>/);
    });

    // INBOX_URL_REGEXP drops these sources, so what is left is the alt text, never an <img>.
    it.each([
      ["a protocol-relative address", '<img src="//host.example/a.png" alt="chart">'],
      ["a backslash address", '<img src="\\\\host.example\\a.png" alt="chart">'],
      ["a relative address", '<img src="a.png" alt="chart">'],
    ])("never keeps an image with %s", (_form, html) => {
      const out = sanitizeInboxHtml(html);
      expect(out).not.toContain("<img");
      expect(out).toContain("chart");
    });

    it("shows an image with no source as its alt text", () => {
      const out = sanitizeInboxHtml('<p><img alt="chart"></p>');
      expect(out).not.toContain("<img");
      expect(out).toContain("chart");
    });
  });

  describe("<table> containment", () => {
    // The sanitizer itself doesn't wrap — the renderer (SanitizedHtml) does.
    // We assert the sanitizer keeps the <table> intact so the wrapper has
    // a target.
    it("keeps <table> structure intact for the render-time wrapper", () => {
      const out = sanitizeInboxHtml(
        "<table><thead><tr><th>a</th></tr></thead><tbody><tr><td>" +
          "x".repeat(400) +
          "</td></tr></tbody></table>",
      );
      expect(out).toMatch(/<table/);
      expect(out).toMatch(/<\/table>/);
      expect(out).toContain("x".repeat(400));
    });
  });
});

describe("InboxMessageContent", () => {
  it("plain renders text verbatim with preserved whitespace", () => {
    render(<InboxMessageContent text={"line one\nline two"} contentType="plain" />);
    const body = screen.getByTestId("inbox-message-body");
    expect(body).toHaveAttribute("data-content-type", "plain");
    expect(body.textContent).toContain("line one");
    expect(body.textContent).toContain("line two");
  });

  it("plain body carries the .inbox-html perimeter class", () => {
    render(<InboxMessageContent text="x" contentType="plain" />);
    const body = screen.getByTestId("inbox-message-body");
    expect(body.className).toContain("inbox-html");
  });

  it("markdown is the default and renders basic markdown", () => {
    render(<InboxMessageContent text="# Hello" />);
    const body = screen.getByTestId("inbox-message-body");
    expect(body).toHaveAttribute("data-content-type", "markdown");
  });

  it("markdown shows a remote image as a link, never loading it", () => {
    render(<InboxMessageContent text="![chart](https://host.example/a.png)" />);
    const body = screen.getByTestId("inbox-message-body");
    expect(body.querySelector("img")).toBeNull();
    expect(screen.getByRole("link", { name: "chart" })).toHaveAttribute(
      "href",
      "https://host.example/a.png",
    );
  });

  it.each([
    ["a relative address", "![chart](a.png)"],
    ["a protocol-relative address", "![chart](//host.example/a.png)"],
  ])("markdown shows an image with %s as a link", (_form, text) => {
    render(<InboxMessageContent text={text} />);
    const body = screen.getByTestId("inbox-message-body");
    expect(body.querySelector("img")).toBeNull();
    expect(screen.getByRole("link", { name: "chart" })).toBeInTheDocument();
  });

  it("markdown body carries the .inbox-html perimeter class", () => {
    render(<InboxMessageContent text="# Hello" />);
    const body = screen.getByTestId("inbox-message-body");
    expect(body.className).toContain("inbox-html");
  });

  it("html renders only the sanitized subset", () => {
    render(
      <InboxMessageContent
        text='<p>ok</p><script>alert(1)</script><a href="javascript:1">bad</a>'
        contentType="html"
      />,
    );
    const body = screen.getByTestId("inbox-message-body");
    expect(body).toHaveAttribute("data-content-type", "html");
    // jsdom supports attachShadow, so the primary path is used — the
    // sanitized content lives inside the shadow tree. The host carries the
    // "shadow" render-path marker.
    const renderPath = body.getAttribute("data-render-path");
    const host = body.querySelector<HTMLDivElement>('[data-inbox-sandbox="shadow"]');
    if (renderPath === "shadow") {
      expect(host).toBeTruthy();
      const inner = host!.shadowRoot?.innerHTML ?? "";
      expect(inner).toContain("<p>ok</p>");
      expect(inner).not.toContain("<script");
      expect(inner).not.toContain("javascript:");
    } else {
      // Fallback path: HTML lives on the light DOM inside the <section>.
      expect(body.innerHTML).toContain("<p>ok</p>");
      expect(body.innerHTML).not.toContain("<script");
      expect(body.innerHTML).not.toContain("javascript:");
    }
  });

  describe("Shadow DOM primary path", () => {
    it("renders sanitized HTML inside the shadow tree, not the light DOM", () => {
      render(<InboxMessageContent text="<p>inside-shadow</p>" contentType="html" />);
      const body = screen.getByTestId("inbox-message-body");
      expect(body.getAttribute("data-render-path")).toBe("shadow");
      const host = body.querySelector<HTMLDivElement>('[data-inbox-sandbox="shadow"]');
      expect(host).toBeTruthy();
      // Light DOM must not carry the shadow children.
      expect(body.textContent).not.toContain("inside-shadow");
      // Shadow tree does.
      expect(host!.shadowRoot?.textContent).toContain("inside-shadow");
    });

    it("wraps a <table> inside the shadow root in .inbox-table-scroll", () => {
      const wide = "x".repeat(400);
      render(
        <InboxMessageContent
          text={`<table><tr><td>${wide}</td></tr></table>`}
          contentType="html"
        />,
      );
      const body = screen.getByTestId("inbox-message-body");
      const host = body.querySelector<HTMLDivElement>('[data-inbox-sandbox="shadow"]');
      const wrap = host?.shadowRoot?.querySelector(".inbox-table-scroll");
      expect(wrap).toBeTruthy();
      // The wrap's immediate child is the table.
      expect(wrap?.firstElementChild?.tagName).toBe("TABLE");
    });

    it("preserves a data:image/png on <img src> inside the shadow root", () => {
      const png = "data:image/png;base64,AAAA";
      render(<InboxMessageContent text={`<img src="${png}" alt="x" />`} contentType="html" />);
      const body = screen.getByTestId("inbox-message-body");
      const host = body.querySelector<HTMLDivElement>('[data-inbox-sandbox="shadow"]');
      const img = host?.shadowRoot?.querySelector("img");
      expect(img).toBeTruthy();
      expect(img?.getAttribute("src")).toBe(png);
    });

    it("strips data:text/html on <a href> before mounting into the shadow root", () => {
      render(
        <InboxMessageContent
          text='<a href="data:text/html,<script>alert(1)</script>">x</a>'
          contentType="html"
        />,
      );
      const body = screen.getByTestId("inbox-message-body");
      const host = body.querySelector<HTMLDivElement>('[data-inbox-sandbox="shadow"]');
      const html = host?.shadowRoot?.innerHTML ?? "";
      expect(html).not.toContain("data:text/html");
      expect(html).not.toContain("<script");
    });
  });

  describe("fallback path (no shadow-DOM support)", () => {
    it("wraps HTML in <section class='inbox-html'> when attachShadow is unavailable", () => {
      // Force the fallback by stubbing attachShadow to throw and the capability
      // check to return false. We swap `Element.prototype.attachShadow` for a
      // function that is NOT present, which trips the `typeof === 'function'`
      // guard in canUseShadowSandbox().
      const originalAttachShadow = (Element.prototype as unknown as { attachShadow?: unknown })
        .attachShadow;
      // Remove the property so `typeof Element.prototype.attachShadow === "function"` is false.

      delete (Element.prototype as any).attachShadow;
      try {
        const wide = "x".repeat(400);
        render(
          <InboxMessageContent
            text={`<table><tr><td>${wide}</td></tr></table>`}
            contentType="html"
          />,
        );
        const body = screen.getByTestId("inbox-message-body");
        expect(body.tagName).toBe("SECTION");
        expect(body.getAttribute("data-render-path")).toBe("fallback");
        expect(body.className).toContain("inbox-html");
        // Author newlines kept, as on the shadow path (inbox-md alone would collapse them).
        expect(body.style.whiteSpace).toBe("pre-wrap");
        // The sanitized + table-wrapped HTML lives directly on the light DOM.
        const wrap = body.querySelector(".inbox-table-scroll");
        expect(wrap).toBeTruthy();
        expect(wrap?.querySelector("table")).toBeTruthy();
      } finally {
        (Element.prototype as any).attachShadow = originalAttachShadow;
      }
    });
  });
});

describe("audit: no `word-break: break-all` leaks into inbox code", () => {
  // Guards against regression — HANDOFF-CHECKLIST § L.14 bans `break-all`
  // across the inbox and agent surfaces. The sanctioned long-token wrap rule
  // is `overflow-wrap: anywhere; word-break: normal`.
  it("`src/features/weft/` has zero `break-all` usages (outside the ban-comment)", () => {
    const root = path.resolve(__dirname, "../");
    const hits = grepBreakAll(root);
    // The ban-comment in `lib/inbox-sandbox.tsx` describes the forbidden
    // value — permit the ONE documentation hit, nothing else.
    const substantive = hits.filter((h) => !h.includes("/lib/inbox-sandbox.tsx"));
    expect(substantive).toEqual([]);
  });

  it("`src/features/agents/` has zero `break-all` usages", () => {
    const root = path.resolve(__dirname, "../../agents");
    const hits = grepBreakAll(root);
    expect(hits).toEqual([]);
  });
});

/**
 * Walk `root` looking for the forbidden `break-all` token in production code.
 * Skips test files (they reference the token in strings and comments by
 * design) and the usual dependency directories.
 */
function grepBreakAll(root: string): string[] {
  const hits: string[] = [];
  const walk = (dir: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === "node_modules" || entry.name === "dist") continue;
        walk(p);
      } else if (/\.(tsx?|css|scss)$/.test(entry.name)) {
        // Skip test files — they call out the ban by name.
        if (/\.(test|seam\.test|spec)\.(tsx?|css|scss)$/.test(entry.name)) continue;
        const content = fs.readFileSync(p, "utf8");
        if (/break-all/.test(content)) hits.push(p);
      }
    }
  };
  if (fs.existsSync(root)) walk(root);
  return hits;
}
