/* =============================================================================
 * create-repo-primer.js — "make an editable context repo first"
 *
 * Why this exists
 *   Some "Try this now" affordances write. The docs org is read-only, so a write
 *   from here is refused by the app. This helper owns that dead end: it shows
 *   where to make an editable context repo, then lets the reader retry.
 *
 *   A page cannot list the reader's orgs or create one (no page-bridge verb does
 *   either), so nothing here detects or creates a repo. The reader makes one with
 *   "Create a new context repo…" (or "Connect a context repo…") in the context
 *   repo menu (⌘⇧G).
 *
 * API (exposed as `window.codesense.docs`)
 *   hasEditableRepo() -> Promise<true>
 *     Always resolves true: the call goes ahead and the app's own refusal shows
 *     the primer when the target is read-only.
 *
 *   ensureEditableRepo() -> Promise (rejects)
 *     Kept for older pages; rejects with the instructions as its message.
 *
 *   mountInlineAffordance(host, { onRetry })
 *     Replaces `host`'s contents with the instructions and a "Try again" button.
 *
 *   mountPersistentCta(host)
 *     Renders a card with the same instructions.
 * ============================================================================= */

(function () {
  "use strict";

  // Resolved against this script's own URL, so it works from any page depth.
  var PRIMER_HREF = (function () {
    try {
      var src = document.currentScript && document.currentScript.src;
      if (src) return new URL("../arc-a-first-touch/create-a-context-repo.html", src).href;
    } catch (_) {
      /* fall through */
    }
    return "arc-a-first-touch/create-a-context-repo.html";
  })();

  var HOW =
    "These docs are read-only. Make your own context repo with " +
    "\u201cCreate a new context repo\u2026\u201d (or \u201cConnect a context repo\u2026\u201d) " +
    "in the context repo menu (\u2318\u21e7G), then try again from a page in it.";

  function hasEditableRepo() {
    return Promise.resolve(true);
  }

  function ensureEditableRepo() {
    return Promise.reject(new Error(HOW));
  }

  function mountInlineAffordance(host, opts) {
    if (!host) return;
    opts = opts || {};
    host.innerHTML =
      '<div class="cs-docs-card" role="status" style="gap: 10px;">' +
      '<p style="margin: 0; color: var(--cs-fg);">' +
      escapeAttr(HOW) +
      " " +
      '<a href="' +
      PRIMER_HREF +
      '">How to create one \u2192</a>' +
      "</p>" +
      '<div style="display: flex; gap: 8px;">' +
      '<button type="button" class="cs-docs-btn cs-docs-btn--ghost cs-docs-btn--sm cs-create-repo__inline">' +
      "Try again" +
      "</button>" +
      "</div>" +
      "</div>";
    var btn = host.querySelector(".cs-create-repo__inline");
    btn.addEventListener("click", function () {
      if (typeof opts.onRetry === "function") opts.onRetry(null);
    });
  }

  function mountPersistentCta(host) {
    if (!host) return;
    host.innerHTML =
      '<div class="cs-docs-card" style="gap: 10px;">' +
      '<h3 class="cs-docs-card__title" style="margin: 0;">Create your context repo</h3>' +
      '<p class="cs-docs-card__hook" style="margin: 0;">' +
      escapeAttr(HOW) +
      "</p>" +
      '<div><a class="cs-docs-btn cs-docs-btn--primary cs-docs-btn--sm" href="' +
      PRIMER_HREF +
      '">How to create one</a></div>' +
      "</div>";
  }

  // ---------------------------------------------------------------------------
  // Small helpers
  // ---------------------------------------------------------------------------
  function escapeAttr(s) {
    return String(s || "")
      .replace(/&/g, "&amp;")
      .replace(/"/g, "&quot;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
  }

  // Expose on window.codesense.docs — namespaced so it never collides with the
  // viewer-vended bridge surface.
  var cs = (window.codesense = window.codesense || {});
  cs.docs = cs.docs || {};
  cs.docs.ensureEditableRepo = ensureEditableRepo;
  cs.docs.mountInlineAffordance = mountInlineAffordance;
  cs.docs.mountPersistentCta = mountPersistentCta;
  cs.docs.hasEditableRepo = hasEditableRepo;
})();
