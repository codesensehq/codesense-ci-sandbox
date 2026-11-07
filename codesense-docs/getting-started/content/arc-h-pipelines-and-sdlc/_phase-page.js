// =============================================================================
// _phase-page.js — Arc H phase-page runtime
//
// Every file in arc-h-pipelines-and-sdlc/N-*.html declares a phase number via
// <body data-phase="N"> and ships a shell of static copy (worked-example prose,
// local-vs-remote callout, try-this affordance). The dynamic facets — agent,
// skill, MCP tools, inbox role, routine trigger, dashboard, "what goes wrong",
// worked-example snippet from phases.yml, and the copy-pasteable agent.yaml +
// routine.yml snippets — are rendered from the single authored source,
// ./../generated/phases.yml.
//
// This is the arc-H counterpart to the renderer inside factory-playbook.html:
// one source, two surfaces, zero drift (I-7 single-authored-source rule).
//
// No external deps. The YAML parser is the exact same tiny block-mapping
// subset the factory-playbook dashboard uses — just enough shape for
// phases.yml's layout (list of maps, nested `agent` map, `|` block scalars for
// prose + yaml-blob fields). If phases.yml ever grows into general YAML this
// parser will need swapping for a full YAML parser.
// =============================================================================

(function () {
  "use strict";

  // The MCP tools below link into the mcp-explorer dashboard's #tool/<name>
  // deep-link; see mcp-explorer.html's hashchange handler. Each phase page
  // emits a chip per tool with that href.
  var MCP_EXPLORER = "../mcp-explorer.html";
  var PHASES_YML = "../generated/phases.yml";

  // -------------------------------------------------------------------------
  // Tiny YAML parser — the subset phases.yml uses. Lifted from
  // factory-playbook.html's parsePhasesYaml so the two surfaces behave the
  // same way on the same input.
  // -------------------------------------------------------------------------
  function parsePhasesYaml(text) {
    var lines = text.replace(/\r\n/g, "\n").split("\n");
    var i = 0;
    while (i < lines.length && !/^phases\s*:\s*$/.test(lines[i])) i++;
    i++;
    var entries = [];
    while (i < lines.length) {
      var line = lines[i];
      if (!line.trim()) {
        i++;
        continue;
      }
      if (/^\S/.test(line)) break;
      var m = /^(\s*)-\s+(.+)$/.exec(line);
      if (!m) {
        i++;
        continue;
      }
      var baseIndent = m[1].length + 2;
      var entry = {};
      var firstKv = /^([a-zA-Z_][\w-]*)\s*:\s*(.*)$/.exec(m[2]);
      if (firstKv) entry[firstKv[1]] = parseScalar(firstKv[2]);
      i++;
      while (i < lines.length) {
        var cur = lines[i];
        if (!cur.trim()) {
          i++;
          continue;
        }
        var curIndent = cur.length - cur.replace(/^\s*/, "").length;
        if (curIndent < baseIndent) break;
        var kv = /^(\s*)([a-zA-Z_][\w-]*)\s*:\s*(.*)$/.exec(cur);
        if (!kv) {
          i++;
          continue;
        }
        var keyIndent = kv[1].length;
        if (keyIndent !== baseIndent) {
          i++;
          continue;
        }
        var key = kv[2];
        var val = kv[3];
        if (val === "" || val === "|" || val === ">") {
          var j = i + 1;
          while (j < lines.length && !lines[j].trim()) j++;
          if (j >= lines.length) {
            entry[key] = val === "|" || val === ">" ? "" : {};
            i++;
            break;
          }
          var nextIndent = lines[j].length - lines[j].replace(/^\s*/, "").length;
          if (val === "|" || val === ">") {
            var block = [];
            i = j;
            while (i < lines.length) {
              var raw = lines[i];
              if (!raw.trim()) {
                block.push("");
                i++;
                continue;
              }
              var ind = raw.length - raw.replace(/^\s*/, "").length;
              if (ind < nextIndent) break;
              block.push(raw.slice(nextIndent));
              i++;
            }
            while (block.length && block[block.length - 1] === "") block.pop();
            entry[key] = block.join("\n") + (val === "|" ? "\n" : "");
          } else {
            var sub = {};
            i = j;
            while (i < lines.length) {
              var r = lines[i];
              if (!r.trim()) {
                i++;
                continue;
              }
              var subInd = r.length - r.replace(/^\s*/, "").length;
              if (subInd < nextIndent) break;
              if (subInd !== nextIndent) {
                i++;
                continue;
              }
              var skv = /^(\s*)([a-zA-Z_][\w-]*)\s*:\s*(.*)$/.exec(r);
              if (!skv) {
                i++;
                continue;
              }
              sub[skv[2]] = parseScalar(skv[3]);
              i++;
            }
            entry[key] = sub;
          }
        } else if (val.indexOf("[") === 0) {
          entry[key] = val
            .replace(/^\[|\]$/g, "")
            .split(",")
            .map(function (s) {
              return parseScalar(s.trim());
            })
            .filter(function (s) {
              return s !== "";
            });
          i++;
        } else {
          entry[key] = parseScalar(val);
          i++;
        }
      }
      entries.push(entry);
    }
    return entries;
  }

  function parseScalar(raw) {
    var s = String(raw || "").trim();
    if (!s) return "";
    if (/^(true|false)$/i.test(s)) return s.toLowerCase() === "true";
    if (/^-?\d+$/.test(s)) return Number(s);
    if (/^".*"$/.test(s)) return s.slice(1, -1);
    if (/^'.*'$/.test(s)) return s.slice(1, -1);
    var hash = s.indexOf(" #");
    if (hash > 0) s = s.slice(0, hash).trim();
    return s;
  }

  function escapeHtml(s) {
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  // -------------------------------------------------------------------------
  // List-of-items under 'phases:' as read from phases.yml. Lazy-loaded; cached
  // on first fetch.
  // -------------------------------------------------------------------------
  var _phasesPromise = null;
  function loadPhases() {
    if (_phasesPromise) return _phasesPromise;
    _phasesPromise = fetch(PHASES_YML, { cache: "no-store" })
      .then(function (r) {
        if (!r.ok) throw new Error("phases.yml HTTP " + r.status);
        return r.text();
      })
      .then(parsePhasesYaml);
    return _phasesPromise;
  }

  // -------------------------------------------------------------------------
  // Render a phase into the page's placeholders.
  //
  // The page's body carries `data-phase="N"`; every injection target has a
  // `data-cs-phase-*` attribute. Any target the current page omits is simply
  // ignored (so the Phase-8 page can skip the trigger glyph if it wants, etc).
  // -------------------------------------------------------------------------
  function render(phase) {
    setText("data-cs-phase-number", String(phase.phase).padStart(2, "0"));
    setText("data-cs-phase-group", String(phase.group || "").toUpperCase());
    setText("data-cs-phase-name", phase.name || "");
    setText("data-cs-phase-skill", phase.skill || "");
    setText("data-cs-phase-trigger", phase.trigger || "");
    setText("data-cs-phase-inbox-role", phase.inboxRole || "");
    setText("data-cs-phase-dashboard", phase.dashboard || "");
    setText(
      "data-cs-phase-agent-name",
      (phase.agent && phase.agent.name) || "",
    );
    setText(
      "data-cs-phase-agent-role",
      (phase.agent && phase.agent.role) || "",
    );

    renderMcpList(phase.mcpTools || []);
    renderPre("data-cs-phase-agent-yaml", phase.agentYaml || "");
    renderPre("data-cs-phase-routine-yml", phase.routineYml || "");
    renderPara("data-cs-phase-what-goes-wrong", phase.whatGoesWrong || "");
    renderPara("data-cs-phase-worked-example", phase.workedExample || "");

    // Dashboard preview — iframe link into the sibling dashboard. We default
    // to a plain anchor + the dashboard name; where a slot exists for an
    // iframe we also attach the src.
    var dashHref = dashboardHref(phase.dashboard);
    document.querySelectorAll("[data-cs-phase-dashboard-link]").forEach(function (el) {
      if (!dashHref) {
        el.textContent = phase.dashboard || "—";
        return;
      }
      el.setAttribute("href", dashHref);
      if (!el.textContent || !el.textContent.trim()) {
        el.textContent = phase.dashboard;
      }
    });
    document.querySelectorAll("[data-cs-phase-dashboard-iframe]").forEach(function (frame) {
      if (dashHref) frame.setAttribute("src", dashHref);
    });

    // Phase-N cross-link into the live factory-playbook dashboard.
    document.querySelectorAll("[data-cs-phase-xref]").forEach(function (el) {
      el.setAttribute("href", "../factory-playbook.html#phase-" + phase.phase);
    });

    // Fade in the body once content lands so there's no flash of placeholder
    // ellipses. Respects reduced-motion via CSS (opacity-only transition).
    document.body.setAttribute("data-cs-ready", "true");
  }

  function setText(attr, value) {
    document.querySelectorAll("[" + attr + "]").forEach(function (el) {
      el.textContent = value;
    });
  }

  function renderPre(attr, value) {
    document.querySelectorAll("[" + attr + "]").forEach(function (el) {
      el.textContent = value.replace(/\n+$/, "");
    });
  }

  function renderPara(attr, value) {
    document.querySelectorAll("[" + attr + "]").forEach(function (el) {
      // Fold the YAML block paragraphs into real paragraphs.
      el.innerHTML = "";
      String(value || "")
        .split(/\n{2,}/)
        .map(function (p) {
          return p.trim();
        })
        .filter(Boolean)
        .forEach(function (p) {
          var para = document.createElement("p");
          // Soft-wrap single newlines (YAML paragraph wrap) into spaces.
          para.textContent = p.replace(/\s*\n\s*/g, " ");
          el.appendChild(para);
        });
    });
  }

  function renderMcpList(tools) {
    document.querySelectorAll("[data-cs-phase-mcp-tools]").forEach(function (host) {
      host.innerHTML = "";
      if (!tools.length) {
        host.textContent = "—";
        return;
      }
      tools.forEach(function (name) {
        var a = document.createElement("a");
        a.className = "cs-docs-chip";
        a.setAttribute("href", MCP_EXPLORER + "#tool/" + name);
        a.textContent = name;
        host.appendChild(a);
        host.appendChild(document.createTextNode(" "));
      });
    });
  }

  function dashboardHref(name) {
    if (!name) return null;
    // Dashboards that live as .html siblings: route to the sibling page.
    if (/\.html$/.test(name)) return "../" + name;
    // "design-canvas" / "release-doc" don't map 1:1 to a shipped file today.
    // Those stay as plain-text labels (handled above in setText), so return
    // null to keep the link inert.
    return null;
  }

  // -------------------------------------------------------------------------
  // Copy-button plumbing — any <button data-cs-copy-target="selector"> copies
  // the target's textContent to the clipboard, falling back to a visible
  // "select all" selection for file:// previews where clipboard access is
  // blocked. Keeps the Try-this-now story real without reaching for the
  // bridge for a snippet copy.
  // -------------------------------------------------------------------------
  function wireCopyButtons() {
    document.querySelectorAll("[data-cs-copy-target]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        var sel = btn.getAttribute("data-cs-copy-target");
        var target = document.querySelector(sel);
        if (!target) return;
        var text = target.textContent || "";
        var done = function () {
          var prev = btn.textContent;
          btn.textContent = "Copied";
          btn.setAttribute("aria-live", "polite");
          setTimeout(function () {
            btn.textContent = prev;
          }, 1400);
        };
        if (navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(text).then(done).catch(selectFallback);
        } else {
          selectFallback();
        }
        function selectFallback() {
          var range = document.createRange();
          range.selectNodeContents(target);
          var selection = window.getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
          done();
        }
      });
    });
  }

  // -------------------------------------------------------------------------
  // Sidenav + stepper rendering — both read phases.yml, so wire them to the
  // same promise. Host elements opt in via `data-cs-arc-h-sidenav` /
  // `data-cs-arc-h-stepper`.
  // -------------------------------------------------------------------------
  function phaseHref(n) {
    return String(n).padStart(2, "0") + "-" + phaseSlug(n);
  }
  // Slug table mirrors the on-disk filenames. Each page knows its own slug;
  // this lookup is only used by index.html and the sidenav for cross-links.
  var PHASE_SLUG = {
    1: "idea",
    2: "spec",
    3: "architecture",
    4: "ux-design",
    5: "stories",
    6: "sprint-plan",
    7: "atdd",
    8: "build",
    9: "decisions",
    10: "review",
    11: "qa",
    12: "retro",
    13: "ship",
    14: "loop",
  };
  function phaseSlug(n) {
    return PHASE_SLUG[n] || "";
  }

  function renderSidenav(phases, current) {
    document.querySelectorAll("[data-cs-arc-h-sidenav]").forEach(function (host) {
      var ol = document.createElement("ol");
      phases.forEach(function (p) {
        var li = document.createElement("li");
        var a = document.createElement("a");
        a.setAttribute("href", phaseHref(p.phase) + ".html");
        if (Number(p.phase) === current) a.setAttribute("aria-current", "page");
        a.innerHTML =
          '<span class="num">' +
          String(p.phase).padStart(2, "0") +
          '</span><span class="name">' +
          escapeHtml(p.name) +
          "</span>";
        li.appendChild(a);
        ol.appendChild(li);
      });
      host.innerHTML = "";
      var label = document.createElement("div");
      label.className = "cs-h-sidenav__label";
      label.textContent = "14 phases";
      host.appendChild(label);
      host.appendChild(ol);
    });
  }

  function renderStepper(phases, current) {
    document.querySelectorAll("[data-cs-arc-h-stepper]").forEach(function (host) {
      host.innerHTML = "";
      phases.forEach(function (p) {
        var a = document.createElement("a");
        a.className = "cs-h-step";
        a.setAttribute("href", phaseHref(p.phase) + ".html");
        a.setAttribute("data-group", p.group || "Plan");
        if (Number(p.phase) === current) a.setAttribute("aria-current", "page");
        a.innerHTML =
          '<span class="cs-h-step__num">' +
          String(p.phase).padStart(2, "0") +
          "</span>" +
          '<span class="cs-h-step__name">' +
          escapeHtml(p.name) +
          "</span>";
        host.appendChild(a);
      });
    });
  }

  // -------------------------------------------------------------------------
  // Boot
  // -------------------------------------------------------------------------
  function boot() {
    var phase = Number(document.body.getAttribute("data-phase"));
    wireCopyButtons();
    loadPhases()
      .then(function (phases) {
        renderSidenav(phases, phase);
        renderStepper(phases, phase);
        if (!phase) return;
        var match = phases.find(function (p) {
          return Number(p.phase) === phase;
        });
        if (!match) throw new Error("phase " + phase + " missing from phases.yml");
        render(match);
      })
      .catch(function (err) {
        var pitch = document.querySelector("[data-cs-phase-worked-example]");
        if (pitch) {
          pitch.innerHTML =
            "<p style=\"color: var(--cs-danger)\">Could not load " +
            escapeHtml(PHASES_YML) +
            ". " +
            escapeHtml(err && err.message ? err.message : String(err)) +
            "</p>";
        }
      });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
