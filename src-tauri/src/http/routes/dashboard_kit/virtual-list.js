// SPDX-License-Identifier: MIT
/* CodeSense kit virtual list. Topic mode requests a live-hub window; local mode
 * windows the page's flattened rows. One keyed renderer owns both modes. */
(function () {
  var cs = window.codesense;
  if (!cs || !cs.kit) return;
  function virtualList(opts) {
    opts = opts || {};
    var box = opts.container, topic = opts.topic, h = opts.rowHeight, render = opts.render;
    var local = Array.isArray(opts.rows), rows = local ? opts.rows : [];
    var tree = !!opts.tree, roots = rows, expanded = new Set(opts.expanded || []), focusId = null, typed = "", typedAt = 0;
    if (tree && !local) throw new TypeError("virtualList: tree mode requires local rows");
    if (tree) box.setAttribute("role", "tree");
    function branch(row) { return Array.isArray(row.children) || row.expandable === true; }
    function flatten(nodes, depth, parent, result) {
      var size = nodes.filter(function (row) { return !row.ariaHidden; }).length, position = 0;
      nodes.forEach(function (row) {
        if (!row.ariaHidden) position++;
        result.push(Object.assign({}, row, { setSize: size, posInSet: position, depth: depth, parentId: parent, expanded: branch(row) ? expanded.has(row.id) : undefined }));
        if (expanded.has(row.id)) flatten(row.children || [], depth + 1, row.id, result);
      });
      return result;
    }
    function treeRender(row, i) {
      var el = render(row, i);
      if (tree && el.getAttribute("aria-hidden") !== "true")
        el.addEventListener("focus", function () { focusId = el.dataset.key; rove(); });
      return el;
    }
    function treeAttrs(el, row) {
      if (!tree || el.getAttribute("aria-hidden") === "true") return;
      el.setAttribute("aria-setsize", String(row.setSize));
      el.setAttribute("aria-posinset", String(row.posInSet));
      el.setAttribute("role", "treeitem");
      el.setAttribute("aria-level", String(row.depth + 1));
      if (branch(row)) el.setAttribute("aria-expanded", String(expanded.has(row.id)));
      else el.removeAttribute("aria-expanded");
      el.style.paddingInlineStart = (6 + row.depth * (opts.indent == null ? 12 : opts.indent)) + "px";
      el.tabIndex = -1;
    }
    function rove() {
      if (!tree || !sizer.children.length) return;
      var current = Array.prototype.find.call(sizer.children, function (el) { return el.dataset.key === focusId && el.getAttribute("role") === "treeitem"; }) || sizer.querySelector('[role="treeitem"]');
      Array.prototype.forEach.call(sizer.children, function (el) { if (el.getAttribute("aria-hidden") !== "true") el.tabIndex = el === current ? 0 : -1; });
    }
    function focusIndex(index) {
      index = Math.max(0, Math.min(index, rows.length - 1));
      if (!rows[index]) return;
      focusId = rows[index].id;
      scrollToIndex(index);
      rove();
      var el = Array.prototype.find.call(sizer.children, function (el) { return el.dataset.key === focusId; });
      if (el) el.focus({ preventScroll: true });
    }
    function toggle(id, open) {
      if (open == null) open = !expanded.has(id);
      if (open) expanded.add(id); else expanded.delete(id);
      setRows(roots);
      if (opts.onExpandedChange) opts.onExpandedChange(Array.from(expanded), id, open);
    }
    function keydown(event) {
      if (!tree || event.target.closest("input,textarea")) return;
      var i = rows.findIndex(function (row) { return row.id === focusId; });
      if (i < 0) i = 0;
      var row = rows[i]; if (!row) return;
      var next = i, key = event.key;
      if (key === "ArrowDown") next++;
      else if (key === "ArrowUp") next--;
      else if (key === "Home") next = 0;
      else if (key === "End") next = rows.length - 1;
      else if (key === "PageDown" || key === "PageUp") next += (key === "PageDown" ? 1 : -1) * Math.max(1, Math.floor(box.clientHeight / (typeof h === "number" ? h : 24)));
      else if (key === "ArrowRight") {
        if (branch(row) && !expanded.has(row.id)) toggle(row.id, true);
        else if (rows[i + 1] && rows[i + 1].parentId === row.id) next++;
      } else if (key === "ArrowLeft") {
        if (branch(row) && expanded.has(row.id)) toggle(row.id, false);
        else if (row.parentId) next = rows.findIndex(function (r) { return r.id === row.parentId; });
      } else if (key === "Enter") {
        if (branch(row)) toggle(row.id);
        else if (opts.onActivate) opts.onActivate(row, event);
      } else if (key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) {
        var now = Date.now(); typed = (now - typedAt > 500 ? "" : typed) + key.toLocaleLowerCase(); typedAt = now;
        for (var step = 1; step <= rows.length; step++) {
          var at = (i + step) % rows.length;
          if (String(rows[at].label || rows[at].name || "").toLocaleLowerCase().startsWith(typed)) { next = at; break; }
        }
      } else return;
      event.preventDefault(); event.stopPropagation(); focusIndex(next);
    }
    var scope = opts.collection === undefined ? undefined : { collection: opts.collection };
    var over = opts.overscan == null ? 8 : opts.overscan;
    if (!box || (!local && (!topic || !(h > 0))) || (local && !(h > 0) && typeof h !== "function") || typeof render !== "function")
      throw new TypeError("virtualList: pass {container, topic or rows, rowHeight, render}");
    var sizer = document.createElement("div");
    sizer.style.position = "relative";
    sizer.setAttribute("data-cs-virtual-list", "");
    box.appendChild(sizer);
    var win = null, total = local ? rows.length : null, frame = 0, dead = false, offsets = [0];
    function indexAt(y) {
      var lo = 0, hi = rows.length;
      while (lo < hi) { var mid = (lo + hi) >>> 1; if (offsets[mid + 1] <= y) lo = mid + 1; else hi = mid; }
      return lo;
    }
    function range() {
      var first = local ? indexAt(box.scrollTop) : Math.floor(box.scrollTop / h);
      var to = local ? indexAt(box.scrollTop + box.clientHeight) + 1 + over : first + Math.ceil(box.clientHeight / h) + 1 + over;
      if (total !== null) to = Math.min(to, total);
      return [Math.max(0, Math.min(first, to) - over), to];
    }
    function place(el, i) {
      el.style.position = "absolute";
      el.style.left = "0";
      el.style.right = "0";
      el.style.top = (local ? offsets[i] : i * h) + "px";
      el.style.height = (local ? offsets[i + 1] - offsets[i] : h) + "px";
    }
    function paint(snapshot) {
      if (dead || !snapshot || !Array.isArray(snapshot.snapshot)) return;
      var from = win ? win[0] : 0;
      total = typeof snapshot.total === "number" ? snapshot.total : from + snapshot.snapshot.length;
      sizer.style.height = (local ? offsets[rows.length] : total * h) + "px";
      // Reconciliation can blur a retained control when its row moves.
      var active = document.activeElement, owner = active && active.closest("[data-key]");
      var focusKey = owner && sizer.contains(owner) ? owner.dataset.key : null;
      var focusControl = active && active.getAttribute("data-cs-focus-key");
      cs.kit.keyed(sizer, snapshot.snapshot.map(function (row, k) { return { row: row, i: from + k }; }), {
        key: function (item) { return item.row.id; },
        version: function (item) { return opts.layoutKey ? opts.layoutKey(item.row) : JSON.stringify(item.row); },
        render: function (item) { return treeRender(item.row, item.i); },
        update: function (el, item) { if (opts.update) opts.update(el, item.row, item.i); },
      });
      Array.prototype.forEach.call(sizer.children, function (el, k) {
        place(el, from + k);
        treeAttrs(el, snapshot.snapshot[k]);
      });
      if (focusKey && document.activeElement !== active) {
        var next = Array.prototype.find.call(sizer.children, function (el) { return el.dataset.key === focusKey; });
        var control = active.isConnected ? active : next && Array.prototype.find.call(next.querySelectorAll("[data-cs-focus-key]"), function (el) {
          return focusControl && el.getAttribute("data-cs-focus-key") === focusControl;
        });
        if (!control && next) control = next.matches("button, [tabindex]") ? next : next.querySelector('[data-cs-focus-key="main"]') || next.querySelector("button, [tabindex]");
        if (!control && tree) control = Array.prototype.find.call(sizer.children, function (el) { return el.dataset.key === focusId && el.getAttribute("role") === "treeitem"; }) || sizer.querySelector('[role="treeitem"]');
        if (control) control.focus({ preventScroll: true });
      }
      rove();
      if (local && opts.onRangeChange) opts.onRangeChange(snapshot.snapshot);
      if (!local) schedule();
    }
    function update() {
      frame = 0;
      if (dead) return;
      var r = range();
      if (win && r[0] === win[0] && r[1] === win[1]) return;
      win = r;
      if (local) paint({ snapshot: rows.slice(r[0], r[1]), total: rows.length });
      else cs.subscribe(topic, Object.assign({ window: win, onChange: paint }, scope)).catch(function () {});
    }
    function schedule() { if (frame === 0) frame = requestAnimationFrame(update); }
    function setRows(next) {
      if (!local) throw new TypeError("setRows is only available in local-rows mode");
      var oldIndex = rows.findIndex(function (r) { return r.id === focusId; });
      roots = next;
      rows = tree ? flatten(next, 0, null, []) : next;
      if (tree && !rows.some(function (r) { return r.id === focusId; })) focusId = rows[Math.min(Math.max(0, oldIndex), rows.length - 1)]?.id || null;
      total = rows.length; offsets = [0];
      rows.forEach(function (row, i) { offsets.push(offsets[i] + (typeof h === "function" ? h(row, i) : h)); });
      win = null; update();
    }
    box.addEventListener("scroll", schedule, { passive: true });
    if (tree) box.addEventListener("keydown", keydown);
    var resize = typeof ResizeObserver === "function" ? new ResizeObserver(schedule) : null;
    if (resize) resize.observe(box);
    if (local) setRows(rows); else update();
    function scrollToIndex(i) {
        var top = local ? offsets[i] : i * h;
        var bottom = local ? offsets[i + 1] : top + h;
        if (top < box.scrollTop) box.scrollTop = top;
        else if (bottom > box.scrollTop + box.clientHeight) box.scrollTop = bottom - box.clientHeight;
        update();
    }
    return {
      setRows: setRows,
      scrollToIndex: scrollToIndex,
      toggle: toggle,
      setExpanded: function (ids) { expanded = new Set(ids); setRows(roots); },
      focus: function (id) { focusIndex(rows.findIndex(function (r) { return r.id === id; })); },
      reveal: function (id) { var i = rows.findIndex(function (r) { return r.id === id; }); if (i >= 0) scrollToIndex(i); },
      refresh: function () { win = null; schedule(); },
      destroy: function () {
        dead = true; box.removeEventListener("scroll", schedule); box.removeEventListener("keydown", keydown);
        if (resize) resize.disconnect();
        if (frame) cancelAnimationFrame(frame);
        if (!local) cs.unsubscribe(topic, paint, scope);
        sizer.remove();
      },
    };
  }
  cs.kit.virtualList = virtualList;
})();
