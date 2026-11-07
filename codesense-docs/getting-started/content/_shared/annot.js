/* =============================================================================
 * annot.js — annotated-overlay helper for docs screenshots
 *
 * Usage
 *   <img data-annot="shots/book-icon.annot.json"
 *        src="shots/book-icon-dark.png" alt="…">
 *   <script src="../_shared/annot.js" defer></script>
 *
 *   The helper walks every <img data-annot>, fetches the sidecar JSON, and
 *   overlays an SVG with the circles / arrows / boxes. The image stays
 *   the baseline raster; annotations are a separate layer.
 *
 * Sidecar schema v1 — `.annot.json`
 *   {
 *     "schema": 1,
 *     "annotations": [
 *       { "kind": "circle", "cx": 820, "cy": 240, "r": 36, "caption": "the book icon" },
 *       { "kind": "arrow",  "from": [820,240], "to": [600,320], "caption": "pinned switcher" },
 *       { "kind": "box",    "x": 100, "y": 300, "w": 200, "h": 80, "caption": "search bar" }
 *     ]
 *   }
 *
 *   All coordinates are in the image's native pixel space (the capture
 *   viewport). The overlay SVG uses `viewBox="0 0 naturalWidth naturalHeight"`
 *   so it tracks any CSS resizing.
 *
 * Interactions
 *   - Each shape is focusable (tabindex=0).
 *   - `?` on a focused shape toggles its caption.
 *   - `Esc` dismisses the floating caption.
 *   - Accent chroma comes from `--cs-accent`.
 * ============================================================================= */

(function () {
  "use strict";

  function toArray(nodeList) {
    return Array.prototype.slice.call(nodeList);
  }

  function wrap(img, overlay) {
    // Insert a wrapper around the image + overlay so stacking + positioning
    // works regardless of the host page's CSS.
    var parent = img.parentNode;
    if (!parent) return;
    var wrapper = document.createElement("span");
    wrapper.className = "cs-annot-wrap";
    wrapper.style.position = "relative";
    wrapper.style.display = "inline-block";
    wrapper.style.lineHeight = "0";
    parent.insertBefore(wrapper, img);
    wrapper.appendChild(img);
    wrapper.appendChild(overlay);
  }

  function renderShape(a, idx) {
    var caption = (a.caption || "").replace(/"/g, "&quot;");
    if (a.kind === "circle") {
      return (
        '<g class="cs-annot cs-annot--circle" tabindex="0" data-idx="' +
        idx +
        '" aria-label="' +
        caption +
        '">' +
        '<circle cx="' +
        a.cx +
        '" cy="' +
        a.cy +
        '" r="' +
        a.r +
        '" ' +
        'fill="none" stroke="var(--cs-accent)" stroke-width="3"/>' +
        "</g>"
      );
    }
    if (a.kind === "box") {
      return (
        '<g class="cs-annot cs-annot--box" tabindex="0" data-idx="' +
        idx +
        '" aria-label="' +
        caption +
        '">' +
        '<rect x="' +
        a.x +
        '" y="' +
        a.y +
        '" width="' +
        a.w +
        '" height="' +
        a.h +
        '" ' +
        'rx="6" ry="6" fill="none" stroke="var(--cs-accent)" stroke-width="3"/>' +
        "</g>"
      );
    }
    if (a.kind === "arrow") {
      var fx = a.from[0],
        fy = a.from[1];
      var tx = a.to[0],
        ty = a.to[1];
      // Compute a simple arrow head.
      var dx = tx - fx,
        dy = ty - fy;
      var len = Math.max(1, Math.sqrt(dx * dx + dy * dy));
      var ux = dx / len,
        uy = dy / len;
      var headLen = 14,
        headWide = 7;
      var hx1 = tx - ux * headLen - uy * headWide;
      var hy1 = ty - uy * headLen + ux * headWide;
      var hx2 = tx - ux * headLen + uy * headWide;
      var hy2 = ty - uy * headLen - ux * headWide;
      return (
        '<g class="cs-annot cs-annot--arrow" tabindex="0" data-idx="' +
        idx +
        '" aria-label="' +
        caption +
        '">' +
        '<line x1="' +
        fx +
        '" y1="' +
        fy +
        '" x2="' +
        tx +
        '" y2="' +
        ty +
        '" ' +
        'stroke="var(--cs-accent)" stroke-width="3" stroke-linecap="round"/>' +
        '<polygon points="' +
        tx +
        "," +
        ty +
        " " +
        hx1 +
        "," +
        hy1 +
        " " +
        hx2 +
        "," +
        hy2 +
        '" ' +
        'fill="var(--cs-accent)"/>' +
        "</g>"
      );
    }
    return "";
  }

  function captionBubble(x, y, text) {
    var el = document.createElement("div");
    el.className = "cs-annot-caption";
    el.textContent = text;
    el.style.position = "absolute";
    el.style.left = x + "px";
    el.style.top = y + "px";
    el.style.transform = "translate(-50%, calc(-100% - 10px))";
    el.style.padding = "4px 8px";
    el.style.borderRadius = "6px";
    el.style.background = "var(--cs-surface-raised)";
    el.style.border = "1px solid var(--cs-border)";
    el.style.color = "var(--cs-fg)";
    el.style.font = "inherit";
    el.style.fontSize = "12px";
    el.style.lineHeight = "1.3";
    el.style.whiteSpace = "nowrap";
    el.style.pointerEvents = "none";
    el.style.boxShadow = "var(--cs-docs-shadow-sm)";
    el.style.zIndex = "2";
    return el;
  }

  function captionAnchorFor(shape) {
    // Returns a (naturalX, naturalY) anchor point in the image's pixel space.
    if (shape.kind === "circle") return [shape.cx, shape.cy - shape.r];
    if (shape.kind === "box") return [shape.x + shape.w / 2, shape.y];
    if (shape.kind === "arrow") return shape.to;
    return [0, 0];
  }

  function toDomSpace(img, overlay, px, py) {
    // Convert from the image's natural pixel space to the overlay element's
    // CSS pixel space. Falls back to the SVG size when the image hasn't
    // reported naturalWidth yet.
    var nw = img.naturalWidth || overlay.clientWidth || 1;
    var nh = img.naturalHeight || overlay.clientHeight || 1;
    var scaleX = overlay.clientWidth / nw;
    var scaleY = overlay.clientHeight / nh;
    return [px * scaleX, py * scaleY];
  }

  function hydrate(img, doc) {
    if (!doc || !Array.isArray(doc.annotations)) return;
    var w = img.naturalWidth || img.width || 1600;
    var h = img.naturalHeight || img.height || 900;
    var svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 " + w + " " + h);
    svg.setAttribute("aria-hidden", "false");
    svg.setAttribute("role", "img");
    svg.style.position = "absolute";
    svg.style.inset = "0";
    svg.style.width = "100%";
    svg.style.height = "100%";
    svg.style.pointerEvents = "none";
    svg.innerHTML = doc.annotations.map(renderShape).join("");
    // Shapes themselves want pointer events so they're keyboard-focusable.
    toArray(svg.querySelectorAll(".cs-annot")).forEach(function (g) {
      g.style.pointerEvents = "auto";
      g.setAttribute("focusable", "true");
    });
    wrap(img, svg);

    // Captions: toggle on focus or `?` press.
    var activeCaption = null;
    function clearCaption() {
      if (activeCaption && activeCaption.parentNode)
        activeCaption.parentNode.removeChild(activeCaption);
      activeCaption = null;
    }
    function showCaption(idx) {
      clearCaption();
      var shape = doc.annotations[idx];
      if (!shape || !shape.caption) return;
      var anchor = captionAnchorFor(shape);
      var dom = toDomSpace(img, svg, anchor[0], anchor[1]);
      activeCaption = captionBubble(dom[0], dom[1], shape.caption);
      svg.parentNode.appendChild(activeCaption);
    }
    svg.addEventListener("focusin", function (e) {
      var g = e.target.closest(".cs-annot");
      if (!g) return;
      showCaption(parseInt(g.getAttribute("data-idx"), 10));
    });
    svg.addEventListener("focusout", clearCaption);
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape") clearCaption();
      if (
        e.key === "?" &&
        document.activeElement &&
        document.activeElement.closest &&
        document.activeElement.closest(".cs-annot")
      ) {
        var idx = parseInt(document.activeElement.getAttribute("data-idx"), 10);
        if (activeCaption) clearCaption();
        else showCaption(idx);
      }
    });
  }

  function initOne(img) {
    var sidecar = img.getAttribute("data-annot");
    if (!sidecar) return;
    // Resolve the sidecar relative to the image's src so authors don't need
    // to repeat the directory.
    var url;
    try {
      url = new URL(sidecar, new URL(img.getAttribute("src") || ".", window.location.href));
    } catch (_) {
      return;
    }
    fetch(url.href, { credentials: "same-origin" })
      .then(function (r) {
        return r.ok ? r.json() : null;
      })
      .then(function (doc) {
        if (!doc) return;
        if (img.complete) hydrate(img, doc);
        else
          img.addEventListener(
            "load",
            function () {
              hydrate(img, doc);
            },
            { once: true },
          );
      })
      .catch(function () {
        /* graceful: no annotations rendered. */
      });
  }

  function init() {
    toArray(document.querySelectorAll("img[data-annot]")).forEach(initOne);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }

  window.initDocsAnnot = init;
})();
