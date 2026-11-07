// SPDX-License-Identifier: MIT
/**
 * SW-IB1-A2 — `InboxSandboxedHtml`: the Shadow-DOM render path for human-inbox
 * HTML message bodies.
 *
 * This component is the hard fix for overnight bug #3 — today a dashboard/HTML
 * message body renders in document-world CSS, where author rules (`word-break:
 * break-all`, unbounded `<pre>`/`<table>` widths, oversize `<img>`) leak into
 * the inbox and shred the layout. The sandbox perimeter here is a scoped
 * `shadowRoot: open` on an internal `<div>`, carrying a *constructable*
 * `CSSStyleSheet` with the five sanctioned rules from DESIGN.md
 * § `components.human-inbox.html-containment.rules`:
 *
 *   :host { overflow-wrap: anywhere; word-break: normal; white-space: pre-wrap;
 *           display: block; }
 *   pre   { max-width: 100%; overflow-x: auto; }
 *   .inbox-table-scroll { max-width: 100%; overflow-x: auto; }
 *   img   { max-width: 100%; height: auto; display: block; }
 *   a     { color: var(--accent); text-underline-offset: 3px; }
 *
 * One stylesheet is constructed per module load and shared across every
 * sandbox instance via `document.adoptedStyleSheets`. On browsers that do not
 * support constructable stylesheets (older Safari / jsdom / happy-dom in
 * tests), we fall back to injecting a `<style>` node with the same rules into
 * the shadow root — the containment is preserved either way.
 *
 * This component does NOT sanitize. The caller (sanitizeInboxHtml, owned by
 * story C1) is responsible for stripping `<script>`, `<iframe>`, `javascript:`
 * URLs, etc., before passing the HTML in. Keeping sanitize and render apart
 * means we can test each separately and keeps this component dependency-free.
 *
 * Spec:
 *   DESIGN.md § components.human-inbox.html-containment
 *   EXPERIENCE.md § Interaction Primitives § Message bodies
 *   HANDOFF-CHECKLIST § L.7 (Shadow-DOM-primary sandbox path)
 */

import React, { useEffect, useRef } from "react";

/** The five sanctioned sandbox rules, verbatim from DESIGN.md. The ordering
 *  here matches the DESIGN frontmatter so a reader comparing the two sees
 *  them line-up. `var(--accent)` resolves against the inbox-tokens contract
 *  established in story A1; if A1 is not merged yet the var will be unset
 *  and the UA default color renders — that is an intentional soft fallback. */
export const INBOX_SANDBOX_CSS = `
:host { overflow-wrap: anywhere; word-break: normal; white-space: pre-wrap; display: block; }
pre { max-width: 100%; overflow-x: auto; }
.inbox-table-scroll { max-width: 100%; overflow-x: auto; }
img { max-width: 100%; height: auto; display: block; }
a { color: var(--theme-accent-text, var(--accent)); text-underline-offset: 3px; }
`.trim();

/** Lazily-constructed, module-shared stylesheet. We cannot build it at module
 *  load because `CSSStyleSheet` is undefined under SSR and some test envs; we
 *  build on first use inside the browser-only effect. `null` means "we tried
 *  and the host does not support constructable sheets — use the <style> fallback". */
let sharedSheet: CSSStyleSheet | null | undefined = undefined;

function getSharedStylesheet(): CSSStyleSheet | null {
  if (sharedSheet !== undefined) return sharedSheet;
  // Feature-detect: `CSSStyleSheet` must exist AND accept the no-arg
  // constructor (Safari <16.4 threw) AND the host document must expose
  // `adoptedStyleSheets`. All three gate the fast path.
  if (
    typeof CSSStyleSheet === "undefined" ||
    typeof document === "undefined" ||
    !("adoptedStyleSheets" in Document.prototype)
  ) {
    sharedSheet = null;
    return null;
  }
  try {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(INBOX_SANDBOX_CSS);
    sharedSheet = sheet;
    return sheet;
  } catch {
    // Constructable stylesheets are not supported here; signal fallback.
    sharedSheet = null;
    return null;
  }
}

/** Test-only hook so `inbox-sandbox.test.tsx` can force the fallback path
 *  without relying on a specific test runtime's feature-detect answers. */
export function __resetInboxSandboxStylesheetCache(): void {
  sharedSheet = undefined;
}

export interface InboxSandboxedHtmlProps {
  /** The ALREADY-SANITIZED HTML to render inside the shadow root. The caller
   *  (sanitizeInboxHtml, story C1) is responsible for stripping dangerous
   *  tags and URL schemes before passing the string in. */
  html: string;
  /** Optional extra class on the outer host div (for layout parent styling).
   *  Does NOT leak into the shadow tree — Shadow DOM encapsulation holds. */
  className?: string;
}

/** Render HTML inside a scoped Shadow DOM with the inbox sandbox stylesheet
 *  applied. See the file header for the full contract. */
export function InboxSandboxedHtml({
  html,
  className,
}: InboxSandboxedHtmlProps): React.JSX.Element {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const shadowRef = useRef<ShadowRoot | null>(null);
  const fallbackStyleRef = useRef<HTMLStyleElement | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    // Attach once per host. React may re-run the effect on html change, and
    // under StrictMode in dev React invokes the effect twice against the
    // SAME host DOM node — so we also check `host.shadowRoot` directly:
    // `attachShadow` on a host that already hosts a shadow tree throws
    // `DOMException: Shadow root cannot be created on a host which already
    // hosts a shadow tree.` Reusing the existing shadow root is correct
    // (its mode is 'open' per our prior attach) and keeps content stable.
    let shadow = shadowRef.current ?? host.shadowRoot;
    const isFreshAttach = !shadow;
    if (!shadow) {
      shadow = host.attachShadow({ mode: "open" });
    }
    shadowRef.current = shadow;

    if (isFreshAttach) {
      const sheet = getSharedStylesheet();
      if (sheet) {
        // Fast path: adopt the constructable stylesheet. All sandbox
        // instances share the same CSSStyleSheet object, so the browser
        // parses the rules once.
        try {
          shadow.adoptedStyleSheets = [sheet];
        } catch {
          // Some engines expose `adoptedStyleSheets` but reject assignment
          // in a shadow root (edge: embedded Chromium with policy). Fall
          // back to a <style> element so containment still holds.
          injectFallbackStyle(shadow, fallbackStyleRef);
        }
      } else {
        injectFallbackStyle(shadow, fallbackStyleRef);
      }
    }

    // Setting innerHTML on the shadow root replaces the body on each html
    // change. We intentionally use innerHTML (not a sanitize call) because
    // the caller already sanitized; see the file header.
    shadow.innerHTML = "";
    // Re-attach fallback <style> if present (innerHTML clear would nuke it).
    if (fallbackStyleRef.current) {
      shadow.appendChild(fallbackStyleRef.current);
    }
    const container = document.createElement("div");
    container.innerHTML = html;
    // Append children to the shadow root so the host's :host rules apply to
    // the layout containing block.
    while (container.firstChild) {
      shadow.appendChild(container.firstChild);
    }
  }, [html]);

  return <div ref={hostRef} className={className} data-inbox-sandbox="shadow" />;
}

function injectFallbackStyle(
  shadow: ShadowRoot,
  ref: React.MutableRefObject<HTMLStyleElement | null>,
): void {
  const style = document.createElement("style");
  style.setAttribute("data-inbox-sandbox-fallback", "true");
  style.textContent = INBOX_SANDBOX_CSS;
  shadow.appendChild(style);
  ref.current = style;
}
