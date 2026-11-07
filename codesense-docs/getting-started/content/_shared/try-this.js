/* =============================================================================
 * try-this.js — the <cs-try-this> custom element
 *
 * Declarative wrapper for the "Try this now" affordance every content page
 * carries. Renders a button, invokes an MCP verb
 * through the viewer bridge, renders the result inline, and gracefully falls
 * back when the user doesn't have an editable context repo yet (pairing with
 * `create-repo-primer.js`).
 *
 * Example
 *   <cs-try-this
 *     verb="setActiveTeam"
 *     args='{"team": "getting-started"}'
 *     label="Switch to getting-started"
 *     confirm="Switch active team?"
 *     dry-run="true"
 *   ></cs-try-this>
 *
 * Attributes
 *   verb       (required) — MCP bridge verb to invoke.
 *   args       (optional) — JSON-encoded args passed to `invoke`.
 *   label      (required) — button label.
 *   confirm    (optional) — if set, shows a confirm sheet before invoking.
 *   dry-run    (optional) — if "true", renders "would invoke …" without calling.
 *   needs-repo (optional) — if "true" (default), checks that an editable repo
 *                           exists first and shows the primer affordance when
 *                           missing. Set to "false" for read-only verbs that
 *                           work fine against the docs org.
 *
 * The verb is lint-checked at build time against the primitives inventory
 * (see `build-codesense-docs.mjs` — build-time lint). Today the
 * allowlist is hard-coded; once generated/primitives.json ships,
 * it drops in here and in the build-time lint.
 * ============================================================================= */

(function () {
  "use strict";

  if (typeof customElements === "undefined" || customElements.get("cs-try-this")) return;

  function bridge() {
    return window.codesense || null;
  }

  function parseArgs(raw) {
    if (!raw) return {};
    try {
      return JSON.parse(raw);
    } catch (_) {
      return {};
    }
  }

  function escapeHtml(s) {
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function isReadOnlyRefusal(err) {
    var msg = (err && (err.message || err.toString())) || "";
    return /read[- ]?only|refuse/i.test(msg);
  }

  var template =
    '<button type="button" class="cs-docs-btn cs-docs-btn--primary cs-docs-btn--sm" ' +
    "data-cs-try-this-btn></button>" +
    '<div class="cs-try-this__result" data-cs-try-this-result ' +
    'style="margin-top: 10px; font-size: var(--cs-docs-text-sm); color: var(--cs-muted-fg);"></div>' +
    '<div class="cs-try-this__primer" data-cs-try-this-primer hidden ' +
    'style="margin-top: 10px;"></div>';

  function CSTryThis() {
    return Reflect.construct(HTMLElement, [], CSTryThis);
  }
  CSTryThis.prototype = Object.create(HTMLElement.prototype);
  CSTryThis.prototype.constructor = CSTryThis;

  CSTryThis.prototype.connectedCallback = function () {
    if (this._mounted) return;
    this._mounted = true;
    this.innerHTML = template;
    this._btn = this.querySelector("[data-cs-try-this-btn]");
    this._result = this.querySelector("[data-cs-try-this-result]");
    this._primer = this.querySelector("[data-cs-try-this-primer]");
    this._btn.textContent = this.getAttribute("label") || "Try it";
    this._btn.addEventListener("click", this._onClick.bind(this));
  };

  CSTryThis.prototype._invokeVerb = function (verb, args) {
    var cs = bridge();
    if (!cs || typeof cs.invoke !== "function") {
      return Promise.reject(new Error("CodeSense bridge unavailable (run this inside CodeSense)."));
    }
    try {
      return Promise.resolve(cs.invoke(verb, args));
    } catch (err) {
      return Promise.reject(err);
    }
  };

  CSTryThis.prototype._renderResult = function (label, body) {
    this._result.innerHTML =
      "<div><strong>" +
      escapeHtml(label) +
      "</strong></div>" +
      (body
        ? '<pre style="margin: 4px 0 0; overflow: auto; padding: 8px; border: 1px solid var(--cs-border); border-radius: 6px; background: var(--cs-inset);">' +
          escapeHtml(body) +
          "</pre>"
        : "");
  };

  CSTryThis.prototype._renderPrimer = function () {
    this._primer.hidden = false;
    var self = this;
    var mountPrimer =
      (window.codesense && window.codesense.docs && window.codesense.docs.mountInlineAffordance) ||
      null;
    if (mountPrimer) {
      mountPrimer(this._primer, {
        onRetry: function () {
          self._primer.hidden = true;
          self._primer.innerHTML = "";
          self._onClick();
        },
      });
    } else {
      this._primer.innerHTML =
        '<div class="cs-docs-card">' +
        '<p style="margin: 0; color: var(--cs-fg);">' +
        "You need an editable context repo first. " +
        '<a href="./arc-a-first-touch/create-a-context-repo.html">How to create one →</a>' +
        "</p>" +
        "</div>";
    }
  };

  CSTryThis.prototype._onClick = function () {
    var self = this;
    var verb = this.getAttribute("verb");
    var args = parseArgs(this.getAttribute("args"));
    var confirmMsg = this.getAttribute("confirm");
    var dryRun = this.getAttribute("dry-run") === "true";
    var needsRepo = this.getAttribute("needs-repo") !== "false";

    if (!verb) {
      this._renderResult("Error", "cs-try-this is missing a `verb` attribute.");
      return;
    }
    if (dryRun) {
      this._renderResult("Dry run", "would invoke `" + verb + "` with " + JSON.stringify(args));
      return;
    }
    if (confirmMsg && !window.confirm(confirmMsg)) return;

    this._btn.setAttribute("aria-busy", "true");
    var proceed =
      needsRepo && window.codesense && window.codesense.docs
        ? window.codesense.docs.hasEditableRepo()
        : Promise.resolve(true);

    proceed
      .then(function (ok) {
        if (!ok && needsRepo) {
          self._btn.removeAttribute("aria-busy");
          self._renderResult("Needs an editable context repo", "");
          self._renderPrimer();
          return null;
        }
        return self._invokeVerb(verb, args).then(function (res) {
          self._btn.removeAttribute("aria-busy");
          self._renderResult(
            "Invoked " + verb,
            typeof res === "string" ? res : JSON.stringify(res, null, 2),
          );
          return res;
        });
      })
      .catch(function (err) {
        self._btn.removeAttribute("aria-busy");
        if (isReadOnlyRefusal(err)) {
          self._renderResult(
            "Refused by the docs org (read-only)",
            "Create an editable context repo and try again.",
          );
          self._renderPrimer();
          return;
        }
        self._renderResult("Error", (err && (err.message || String(err))) || "unknown");
      });
  };

  Object.setPrototypeOf(CSTryThis, HTMLElement);
  customElements.define("cs-try-this", CSTryThis);
})();
