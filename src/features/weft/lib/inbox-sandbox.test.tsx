// SPDX-License-Identifier: MIT
import { render } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  __resetInboxSandboxStylesheetCache,
  INBOX_SANDBOX_CSS,
  InboxSandboxedHtml,
} from "./inbox-sandbox";

afterEach(() => {
  __resetInboxSandboxStylesheetCache();
  vi.restoreAllMocks();
});

function getHost(container: HTMLElement): HTMLDivElement {
  const host = container.querySelector<HTMLDivElement>('[data-inbox-sandbox="shadow"]');
  if (!host) throw new Error("sandbox host not found");
  return host;
}

describe("InboxSandboxedHtml — Shadow DOM boundary", () => {
  it("mounts an open shadow root on the host div", () => {
    const { container } = render(<InboxSandboxedHtml html="<p>hello</p>" />);
    const host = getHost(container);
    expect(host.shadowRoot).toBeTruthy();
    expect(host.shadowRoot?.mode).toBe("open");
  });

  it("renders the html inside the shadow root (not the light DOM)", () => {
    const { container } = render(<InboxSandboxedHtml html="<p>inside-shadow</p>" />);
    const host = getHost(container);
    // The light DOM does NOT contain the shadow children.
    expect(container.textContent).not.toContain("inside-shadow");
    // The shadow DOM does.
    expect(host.shadowRoot?.textContent).toContain("inside-shadow");
  });

  it("replaces the shadow body when the html prop changes (no leak)", () => {
    const { container, rerender } = render(<InboxSandboxedHtml html="<p>first</p>" />);
    const host = getHost(container);
    expect(host.shadowRoot?.textContent).toContain("first");

    rerender(<InboxSandboxedHtml html="<p>second</p>" />);
    expect(host.shadowRoot?.textContent).not.toContain("first");
    expect(host.shadowRoot?.textContent).toContain("second");
  });

  it("tolerates React StrictMode's double-invoked mount effect (no shadow re-attach throw)", () => {
    // Under <StrictMode> React invokes mount effects twice against the same
    // DOM node. If our effect called `host.attachShadow` unconditionally on
    // the second invocation, it would throw `DOMException: Shadow root
    // cannot be created on a host which already hosts a shadow tree.`
    // Rendering must succeed, and the shadow root must still carry the html.
    expect(() => {
      const { container } = render(
        <StrictMode>
          <InboxSandboxedHtml html="<p>strict</p>" />
        </StrictMode>,
      );
      const host = getHost(container);
      expect(host.shadowRoot?.textContent).toContain("strict");
    }).not.toThrow();
  });

  it("does not call window / global mutation outside its shadow root between renders", () => {
    // We spot-check: no new document.body children are appended by the
    // sandbox render path. (React testing-library owns the root container.)
    const bodyChildrenBefore = document.body.children.length;
    const { rerender } = render(<InboxSandboxedHtml html="<p>a</p>" />);
    rerender(<InboxSandboxedHtml html="<p>b</p>" />);
    rerender(<InboxSandboxedHtml html="<p>c</p>" />);
    // RTL appends exactly one container to document.body for the whole test.
    expect(document.body.children.length).toBeLessThanOrEqual(bodyChildrenBefore + 1);
  });
});

describe("InboxSandboxedHtml — constructable stylesheet (fast path)", () => {
  it("applies the shared CSSStyleSheet via adoptedStyleSheets when supported", () => {
    // happy-dom supports constructable stylesheets; this exercises the
    // fast path on the shipped test runtime.
    if (typeof CSSStyleSheet === "undefined" || !("adoptedStyleSheets" in Document.prototype)) {
      // Runtime does not expose the API — skip without failing (the
      // fallback path has its own test below).
      return;
    }
    const { container } = render(<InboxSandboxedHtml html="<p>x</p>" />);
    const host = getHost(container);
    const sheets = host.shadowRoot?.adoptedStyleSheets ?? [];
    expect(sheets.length).toBe(1);
    // The sheet's rules must include our containment rule for `pre`.
    const rules = Array.from(sheets[0]!.cssRules).map((r) => r.cssText);
    expect(rules.some((r) => /pre\s*\{/.test(r))).toBe(true);
    expect(rules.some((r) => /overflow-wrap/.test(r))).toBe(true);
  });

  it("shares the same CSSStyleSheet across multiple instances (one parse)", () => {
    if (typeof CSSStyleSheet === "undefined" || !("adoptedStyleSheets" in Document.prototype)) {
      return;
    }
    const { container: c1 } = render(<InboxSandboxedHtml html="<p>a</p>" />);
    const { container: c2 } = render(<InboxSandboxedHtml html="<p>b</p>" />);
    const s1 = getHost(c1).shadowRoot?.adoptedStyleSheets?.[0];
    const s2 = getHost(c2).shadowRoot?.adoptedStyleSheets?.[0];
    expect(s1).toBeDefined();
    expect(s1).toBe(s2); // identity, not just equality — shared sheet
  });
});

describe("InboxSandboxedHtml — <style> fallback (no constructable support)", () => {
  it("injects a <style> node with the sandbox rules when adoptedStyleSheets is unavailable", () => {
    // Force the fallback by temporarily stripping the capability. We cannot
    // delete `adoptedStyleSheets` from `Document.prototype` reliably across
    // runtimes, so we stub `CSSStyleSheet` to a throwing shim — the module
    // treats that identically to "unsupported" and falls back.
    const originalCSSStyleSheet = globalThis.CSSStyleSheet;
    class ThrowingSheet {
      constructor() {
        throw new Error("constructable stylesheets not supported");
      }
    }
    // @ts-expect-error — intentional stub for the fallback path
    globalThis.CSSStyleSheet = ThrowingSheet;
    __resetInboxSandboxStylesheetCache();

    try {
      const { container } = render(<InboxSandboxedHtml html="<p>f</p>" />);
      const host = getHost(container);
      const styleEl = host.shadowRoot?.querySelector<HTMLStyleElement>(
        'style[data-inbox-sandbox-fallback="true"]',
      );
      expect(styleEl).toBeTruthy();
      // The style tag's text must carry our sandbox rules verbatim.
      expect(styleEl?.textContent).toContain("overflow-wrap: anywhere");
      expect(styleEl?.textContent).toContain(".inbox-table-scroll");
      expect(styleEl?.textContent).toBe(INBOX_SANDBOX_CSS);
    } finally {
      globalThis.CSSStyleSheet = originalCSSStyleSheet;
      __resetInboxSandboxStylesheetCache();
    }
  });
});

describe("InboxSandboxedHtml — sanitization boundary (enables, does not restrict)", () => {
  it("does NOT sanitize — raw HTML is rendered as given (caller sanitizes)", () => {
    // The component is explicitly a renderer, not a sanitizer. Agent HTML
    // messages are rich content; sanitize lives in `sanitizeInboxHtml` so
    // callers that already produce safe HTML (e.g. markdown-rendered agent
    // output) are not re-sanitized. This is the enable-don't-restrict shape:
    // we do not gate who may send rich HTML, we just scope its CSS.
    const html = '<p class="agent-note">rich <strong>content</strong></p>';
    const { container } = render(<InboxSandboxedHtml html={html} />);
    const host = getHost(container);
    const strong = host.shadowRoot?.querySelector("strong");
    expect(strong?.textContent).toBe("content");
    const p = host.shadowRoot?.querySelector("p.agent-note");
    expect(p).toBeTruthy();
  });

  it("accepts the pre-wrapped .inbox-table-scroll shape from the sanitizer", () => {
    const html =
      '<div class="inbox-table-scroll"><table><tr><td>' +
      "a".repeat(400) +
      "</td></tr></table></div>";
    const { container } = render(<InboxSandboxedHtml html={html} />);
    const host = getHost(container);
    const wrap = host.shadowRoot?.querySelector(".inbox-table-scroll");
    expect(wrap).toBeTruthy();
    // The shadow root carries a rule for .inbox-table-scroll — assert its
    // presence in whichever path shipped (adopted sheet or <style> fallback).
    const sheets = host.shadowRoot?.adoptedStyleSheets ?? [];
    const styleEl = host.shadowRoot?.querySelector("style[data-inbox-sandbox-fallback]");
    const allRules =
      sheets.flatMap((s) => Array.from(s.cssRules).map((r) => r.cssText)).join("\n") +
      (styleEl?.textContent ?? "");
    expect(allRules).toMatch(/\.inbox-table-scroll/);
  });
});

describe("InboxSandboxedHtml — accepts optional className on the host", () => {
  it("forwards className to the host element without leaking into the shadow tree", () => {
    const { container } = render(<InboxSandboxedHtml html="<p>x</p>" className="my-host-class" />);
    const host = getHost(container);
    expect(host.className).toBe("my-host-class");
    // The class name does NOT appear inside the shadow tree.
    expect(host.shadowRoot?.innerHTML ?? "").not.toContain("my-host-class");
  });
});
