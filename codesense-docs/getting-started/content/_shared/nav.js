/* =============================================================================
 * nav.js — shared docs chrome: top-nav + arc dropdown + ⌘K palette
 *
 * Responsibilities
 *   - Render the top nav into every <nav class="cs-topnav" data-cs-nav></nav>
 *     mount (or into a <nav class="cs-topnav"> that is empty).
 *   - Highlight the current page by reading `document.body.dataset.page`.
 *   - Mount the ⌘K / Ctrl+K command palette.
 *   - Lazy-load `generated/search-index.json` on first palette open.
 *   - Stay theme-aware: subscribe to the viewer's codesense.onTheme when
 *     available; fall back to `prefers-color-scheme` otherwise. Theme toggles
 *     set html[data-theme] and cross-fade via CSS transition (no reload).
 *
 * Shipping shape
 *   - Zero external dependencies. No bundler required.
 *   - Exposes `window.initDocsNav()` and auto-runs on DOMContentLoaded.
 *   - Honors prefers-reduced-motion (via CSS; palette opens as cross-fade only).
 * ============================================================================= */

(function () {
  "use strict";

  // Every href below is relative to the docs content root (the folder above _shared/), so pages in
  // the arc-* folders resolve them the same way as pages at the root.
  var SCRIPT = document.currentScript;
  var ROOT = new URL("../", (SCRIPT && SCRIPT.src) || window.location.href);
  function rootHref(href) {
    if (!href || /^(?:[a-z][a-z0-9+.-]*:|#|\/)/i.test(href)) return href;
    return new URL(href, ROOT).href;
  }

  // ---------------------------------------------------------------------------
  // Arcs — the 14 named stops. Order is canonical and visible in the dropdown.
  // The generator can later rewrite this at build-time; shape stays.
  // ---------------------------------------------------------------------------
  var ARCS = [
    {
      letter: "A",
      slug: "a-first-touch",
      title: "First touch",
      href: "./arc-a-first-touch/index.html",
    },
    { letter: "B", slug: "b-model", title: "The model", href: "./arc-b-model/index.html" },
    {
      letter: "C",
      slug: "c-workspaces",
      title: "Workspaces & worktrees",
      href: "./arc-c-workspaces-and-worktrees/index.html",
    },
    {
      letter: "D",
      slug: "d-terminals",
      title: "Terminals & machines",
      href: "./arc-d-terminals-and-machines/index.html",
    },
    { letter: "E", slug: "e-agents", title: "Agents", href: "./arc-e-agents/index.html" },
    { letter: "F", slug: "f-inbox", title: "Inboxes & triage", href: "./arc-f-inboxes-and-triage/index.html" },
    { letter: "G", slug: "g-routines", title: "Routines", href: "./arc-g-routines/index.html" },
    { letter: "H", slug: "h-factory", title: "Pipelines & the SDLC", href: "./arc-h-pipelines-and-sdlc/index.html" },
    {
      letter: "I",
      slug: "i-primitives",
      title: "Dashboards & primitives",
      href: "./arc-i-dashboard-primitives/index.html",
    },
    { letter: "J", slug: "j-mcp", title: "MCP", href: "./arc-j-mcp/index.html" },
    { letter: "K", slug: "k-review", title: "Sync", href: "./arc-k-sync/index.html" },
    { letter: "L", slug: "l-ship", title: "The factory", href: "./arc-l-factory/index.html" },
    {
      letter: "M",
      slug: "m-crossref",
      title: "UI ↔ MCP crossref",
      href: "./ui-mcp-crossref.html",
    },
    {
      letter: "N",
      slug: "n-reference",
      title: "Reference catalog",
      href: "./arc-n-reference/index.html",
    },
  ];

  // Top-level shortcut links that appear beside the arc dropdown.
  var SHORTCUTS = [
    { page: "home", href: "./README.html", label: "Home" },
    { page: "tour", href: "./tour.html", label: "Tour" },
    { page: "factory-playbook", href: "./factory-playbook.html", label: "Factory" },
    { page: "recipes-gallery", href: "./recipes-gallery.html", label: "Recipes" },
    { page: "schema-browser", href: "./schema-browser.html", label: "Schemas" },
    { page: "architecture", href: "./architecture.html", label: "Architecture" },
  ];

  var isMac = /Mac|iPhone|iPad|iPod/i.test(
    (typeof navigator !== "undefined" && navigator.platform) || "",
  );
  var MOD_KEY = isMac ? "⌘" : "Ctrl";
  var SEARCH_INDEX_URL = rootHref("./generated/search-index.json");

  // ---------------------------------------------------------------------------
  // SVG icons — inlined. No external fetches.
  // ---------------------------------------------------------------------------
  function svg(path, w) {
    w = w || 14;
    return (
      '<svg viewBox="0 0 24 24" width="' +
      w +
      '" height="' +
      w +
      '" ' +
      'fill="none" stroke="currentColor" stroke-width="1.75" ' +
      'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
      path +
      "</svg>"
    );
  }
  var ICON = {
    search: svg('<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/>'),
    chevron: svg('<path d="M6 9l6 6 6-6"/>'),
    sun: svg(
      '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"/>',
    ),
    moon: svg('<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/>'),
    arrow: svg('<path d="M5 12h14M13 6l6 6-6 6"/>', 12),
  };

  // ---------------------------------------------------------------------------
  // 1 · Top nav render
  // ---------------------------------------------------------------------------
  function currentPage() {
    return (document.body && document.body.getAttribute("data-page")) || "";
  }
  function currentArcLetter() {
    var page = currentPage();
    for (var i = 0; i < ARCS.length; i++) {
      if (ARCS[i].slug === page || page.indexOf("arc-" + ARCS[i].letter.toLowerCase()) === 0) {
        return ARCS[i].letter;
      }
    }
    // Dashboards aren't arc pages; the dropdown trigger still shows "Arcs".
    return null;
  }

  function renderTopNav(mount) {
    var activeArc = currentArcLetter();
    var activeArcLabel = activeArc ? "Arc " + activeArc : "Arcs";
    var page = currentPage();

    var shortcutHtml = SHORTCUTS.map(function (s) {
      var current = s.page === page ? ' aria-current="page"' : "";
      return (
        '<a class="cs-topnav__link" href="' + rootHref(s.href) + '"' + current + ">" + s.label + "</a>"
      );
    }).join("");

    var menuHtml = ARCS.map(function (a) {
      var cur = a.letter === activeArc ? ' aria-current="page"' : "";
      return (
        '<a class="cs-topnav__menu-item" href="' +
        rootHref(a.href) +
        '"' +
        cur +
        ' role="menuitem">' +
        '<span class="letter">' +
        a.letter +
        "</span>" +
        "<span>" +
        a.title +
        "</span>" +
        "</a>"
      );
    }).join("");

    mount.innerHTML =
      '<a class="cs-topnav__brand" href="' + rootHref("./README.html") + '" aria-label="CodeSense docs home">' +
      '<span class="cs-topnav__brand-mark" aria-hidden="true"></span>' +
      "<span>CodeSense</span>" +
      '<span class="cs-topnav__brand-slash">/</span>' +
      "<span>Docs</span>" +
      "</a>" +
      '<button class="cs-topnav__arcs" type="button" aria-haspopup="menu" aria-expanded="false" data-cs-arcs>' +
      '<span class="cs-topnav__arcs-letter">' +
      (activeArc || "A–N") +
      "</span>" +
      "<span>" +
      activeArcLabel +
      "</span>" +
      ICON.chevron +
      "</button>" +
      '<div class="cs-topnav__menu" role="menu" data-cs-arcs-menu>' +
      menuHtml +
      "</div>" +
      '<nav class="cs-topnav__shortcuts" aria-label="Shortcuts">' +
      shortcutHtml +
      "</nav>" +
      '<button class="cs-topnav__search" type="button" aria-label="Open search palette" data-cs-palette-open>' +
      ICON.search +
      '<span class="cs-topnav__search-placeholder">Search docs, tools, primitives…</span>' +
      '<span class="cs-docs-kbd">' +
      MOD_KEY +
      "</span>" +
      '<span class="cs-docs-kbd">K</span>' +
      "</button>" +
      '<button class="cs-topnav__theme" type="button" aria-label="Toggle theme" data-cs-theme-toggle>' +
      ICON.moon +
      "</button>";

    wireArcDropdown(mount);
  }

  function wireArcDropdown(mount) {
    var trigger = mount.querySelector("[data-cs-arcs]");
    var menu = mount.querySelector("[data-cs-arcs-menu]");
    if (!trigger || !menu) return;

    function close() {
      menu.setAttribute("data-open", "false");
      trigger.setAttribute("aria-expanded", "false");
    }
    function open() {
      menu.setAttribute("data-open", "true");
      trigger.setAttribute("aria-expanded", "true");
      var first = menu.querySelector(".cs-topnav__menu-item");
      if (first) first.focus();
    }
    trigger.addEventListener("click", function (e) {
      e.stopPropagation();
      var expanded = trigger.getAttribute("aria-expanded") === "true";
      if (expanded) close();
      else open();
    });
    document.addEventListener("click", function (e) {
      if (!menu.contains(e.target) && e.target !== trigger) close();
    });
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape") close();
    });
  }

  // ---------------------------------------------------------------------------
  // 2 · ⌘K palette — overlay + input + list + search-index fetch
  // ---------------------------------------------------------------------------
  var paletteState = {
    overlay: null,
    input: null,
    list: null,
    empty: null,
    items: null,
    results: [],
    selected: 0,
    indexLoadStarted: false,
    index: null,
  };

  function ensurePaletteMounted() {
    if (paletteState.overlay) return paletteState.overlay;
    var overlay = document.createElement("div");
    overlay.className = "cs-palette-overlay";
    overlay.setAttribute("role", "dialog");
    overlay.setAttribute("aria-modal", "true");
    overlay.setAttribute("aria-label", "Search palette");
    overlay.innerHTML =
      '<div class="cs-palette">' +
      '<div class="cs-palette__input-row">' +
      ICON.search +
      '<input class="cs-palette__input" type="search" ' +
      'placeholder="Search every doc, tool, primitive, example…" ' +
      'autocomplete="off" spellcheck="false" aria-label="Search">' +
      '<span class="cs-palette__hint">esc to close</span>' +
      "</div>" +
      '<ul class="cs-palette__list" role="listbox" aria-label="Search results"></ul>' +
      '<div class="cs-palette__empty" hidden>Type to search the docs org.</div>' +
      '<div class="cs-palette__footer">' +
      '<span><span class="cs-docs-kbd">↑</span><span class="cs-docs-kbd">↓</span> move</span>' +
      '<span><span class="cs-docs-kbd">↵</span> open</span>' +
      '<span><span class="cs-docs-kbd">esc</span> close</span>' +
      "</div>" +
      "</div>";
    document.body.appendChild(overlay);
    paletteState.overlay = overlay;
    paletteState.input = overlay.querySelector(".cs-palette__input");
    paletteState.list = overlay.querySelector(".cs-palette__list");
    paletteState.empty = overlay.querySelector(".cs-palette__empty");

    overlay.addEventListener("click", function (e) {
      if (e.target === overlay) closePalette();
    });
    paletteState.input.addEventListener("input", renderPaletteResults);
    paletteState.input.addEventListener("keydown", onPaletteKey);
    return overlay;
  }

  function openPalette() {
    ensurePaletteMounted();
    paletteState.overlay.setAttribute("data-open", "true");
    paletteState.selected = 0;
    paletteState.input.value = "";
    renderPaletteResults();
    // Defer focus until after the overlay reflows so the input truly accepts it.
    requestAnimationFrame(function () {
      paletteState.input.focus();
    });
    loadSearchIndex();
    document.documentElement.style.overflow = "hidden";
  }

  function closePalette() {
    if (!paletteState.overlay) return;
    paletteState.overlay.setAttribute("data-open", "false");
    document.documentElement.style.overflow = "";
  }

  function loadSearchIndex() {
    if (paletteState.indexLoadStarted) return;
    paletteState.indexLoadStarted = true;

    // Resolve the URL relative to the current document, not the chrome script.
    fetch(SEARCH_INDEX_URL, { credentials: "same-origin" })
      .then(function (r) {
        return r.ok ? r.json() : Promise.reject(new Error(r.status));
      })
      .then(function (json) {
        paletteState.index = Array.isArray(json) ? json : (json && json.entries) || [];
        renderPaletteResults();
      })
      .catch(function () {
        // No index yet (standalone preview). Show a graceful empty state.
        paletteState.index = [];
        renderPaletteResults();
      });
  }

  // Very small forgiving fuzzy match. In-order character hits score higher.
  function fuzzyScore(hay, needle) {
    if (!needle) return 0;
    hay = (hay || "").toLowerCase();
    needle = needle.toLowerCase();
    var hi = 0,
      score = 0,
      streak = 0;
    for (var ni = 0; ni < needle.length; ni++) {
      var c = needle.charAt(ni);
      var found = hay.indexOf(c, hi);
      if (found === -1) return -1;
      streak = found === hi ? streak + 1 : 0;
      score += 1 + streak;
      hi = found + 1;
    }
    // Reward prefix hits.
    if (hay.indexOf(needle) === 0) score += 8;
    return score;
  }

  function searchIndex(q) {
    var idx = paletteState.index || [];
    if (!q) {
      // No query → show the arcs + top shortcuts so the palette is useful empty.
      return SHORTCUTS.map(function (s) {
        return { title: s.label, crumb: "Dashboard", kind: "page", href: s.href };
      }).concat(
        ARCS.map(function (a) {
          return { title: a.title, crumb: "Arc " + a.letter, kind: "arc", href: a.href };
        }),
      );
    }
    var scored = [];
    for (var i = 0; i < idx.length; i++) {
      var row = idx[i];
      var title = row.title || row.name || "";
      var crumb = row.crumb || row.section || row.arc || "";
      var titleScore = fuzzyScore(title, q);
      var crumbScore = fuzzyScore(crumb, q);
      var bodyScore = fuzzyScore(row.text || row.body || "", q);
      var best = Math.max(titleScore, crumbScore, bodyScore);
      if (best >= 0) {
        scored.push({
          score: best + (titleScore > 0 ? 10 : 0),
          row: {
            title: title,
            crumb: crumb,
            kind: row.kind || "section",
            href: row.href || row.url || "#",
          },
        });
      }
    }
    scored.sort(function (a, b) {
      return b.score - a.score;
    });
    return scored.slice(0, 24).map(function (s) {
      return s.row;
    });
  }

  function renderPaletteResults() {
    var q = paletteState.input ? paletteState.input.value.trim() : "";
    var results = searchIndex(q);
    paletteState.results = results;
    paletteState.selected = 0;

    if (!results.length) {
      paletteState.list.innerHTML = "";
      paletteState.empty.hidden = false;
      paletteState.empty.textContent = q
        ? 'No matches for "' + q + '".'
        : "Start typing to search docs, tools, primitives.";
      return;
    }
    paletteState.empty.hidden = true;
    paletteState.list.innerHTML = results
      .map(function (r, i) {
        return (
          '<li class="cs-palette__item" role="option" tabindex="-1" ' +
          'data-index="' +
          i +
          '" aria-selected="' +
          (i === 0 ? "true" : "false") +
          '">' +
          '<span class="cs-palette__item-icon">' +
          ICON.arrow +
          "</span>" +
          "<span>" +
          '<div class="cs-palette__item-title">' +
          escapeHtml(r.title) +
          "</div>" +
          (r.crumb ? '<div class="cs-palette__item-crumb">' + escapeHtml(r.crumb) + "</div>" : "") +
          "</span>" +
          '<span class="cs-palette__item-kind">' +
          escapeHtml(r.kind) +
          "</span>" +
          "</li>"
        );
      })
      .join("");
    paletteState.items = paletteState.list.querySelectorAll(".cs-palette__item");

    paletteState.list.addEventListener("click", onPaletteClickOnce, { once: true });
  }

  function onPaletteClickOnce(e) {
    var li = e.target.closest(".cs-palette__item");
    if (!li) return;
    openResult(parseInt(li.getAttribute("data-index"), 10));
  }

  function updateSelected(idx) {
    var items = paletteState.items;
    if (!items || !items.length) return;
    idx = Math.max(0, Math.min(items.length - 1, idx));
    paletteState.selected = idx;
    for (var i = 0; i < items.length; i++) {
      items[i].setAttribute("aria-selected", i === idx ? "true" : "false");
    }
    items[idx].scrollIntoView({ block: "nearest" });
  }

  function openResult(idx) {
    var r = paletteState.results[idx];
    if (!r) return;
    closePalette();
    // Prefer the viewer's openFile when the bridge is present; else navigate.
    var cs = window.codesense;
    if (cs && typeof cs.openFile === "function") {
      // The app resolves openFile paths against the content root, as the hrefs are written.
      try {
        Promise.resolve(cs.openFile({ path: r.href, newTab: false })).catch(function () {
          window.location.href = rootHref(r.href);
        });
        return;
      } catch (_) {}
    }
    window.location.href = rootHref(r.href);
  }

  function onPaletteKey(e) {
    if (e.key === "Escape") {
      e.preventDefault();
      closePalette();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      updateSelected(paletteState.selected + 1);
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      updateSelected(paletteState.selected - 1);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      openResult(paletteState.selected);
      return;
    }
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  // ---------------------------------------------------------------------------
  // 3 · Theme — subscribe to the viewer, else prefers-color-scheme
  // ---------------------------------------------------------------------------
  function applyScheme(scheme) {
    if (!scheme) return;
    document.documentElement.setAttribute("data-theme", scheme);
    // Update the toggle icon if the chrome is mounted.
    var btn = document.querySelector("[data-cs-theme-toggle]");
    if (btn) btn.innerHTML = scheme === "dark" ? ICON.moon : ICON.sun;
  }

  function wireThemeAwareness() {
    var cs = window.codesense;

    // Viewer-vended theme (preferred).
    if (cs && typeof cs.subscribeContext === "function" && typeof cs.onTheme === "function") {
      try {
        cs.onTheme(function (theme) {
          if (theme && theme.scheme) applyScheme(theme.scheme);
        });
      } catch (_) {}
    } else if (cs && typeof cs.onTheme === "function") {
      try {
        cs.onTheme(function (theme) {
          if (theme && theme.scheme) applyScheme(theme.scheme);
        });
      } catch (_) {}
    }

    // Fallback: match the OS when the bridge isn't live.
    try {
      var mq = window.matchMedia("(prefers-color-scheme: dark)");
      applyScheme(mq.matches ? "dark" : "light");
      mq.addEventListener &&
        mq.addEventListener("change", function (e) {
          applyScheme(e.matches ? "dark" : "light");
        });
    } catch (_) {}

    // Manual toggle button.
    document.addEventListener("click", function (e) {
      var btn = e.target.closest("[data-cs-theme-toggle]");
      if (!btn) return;
      var current = document.documentElement.getAttribute("data-theme") || "dark";
      applyScheme(current === "dark" ? "light" : "dark");
    });
  }

  // ---------------------------------------------------------------------------
  // 4 · Global keybinding — ⌘K / Ctrl+K opens the palette
  // ---------------------------------------------------------------------------
  function wireShortcuts() {
    document.addEventListener("keydown", function (e) {
      var mod = isMac ? e.metaKey : e.ctrlKey;
      if (mod && (e.key === "k" || e.key === "K")) {
        e.preventDefault();
        openPalette();
      }
    });
    document.addEventListener("click", function (e) {
      var trigger = e.target.closest("[data-cs-palette-open]");
      if (trigger) {
        e.preventDefault();
        openPalette();
      }
    });
  }

  // ---------------------------------------------------------------------------
  // 5 · Entry
  // ---------------------------------------------------------------------------
  function initDocsNav() {
    // Pick the first unmounted cs-topnav or an opt-in [data-cs-nav].
    var mounts = document.querySelectorAll("nav.cs-topnav, [data-cs-nav]");
    if (!mounts.length) return;

    for (var i = 0; i < mounts.length; i++) {
      // Treat as "shim present" if the author put fallback markup inside; still
      // overwrite with the canonical nav so prod and standalone agree.
      renderTopNav(mounts[i]);
    }

    // Flag the body so authored fallbacks can hide themselves.
    document.body.setAttribute("data-nav-ready", "true");

    wireShortcuts();
    wireThemeAwareness();
  }

  // Expose + auto-run.
  window.initDocsNav = initDocsNav;
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initDocsNav);
  } else {
    initDocsNav();
  }
})();
