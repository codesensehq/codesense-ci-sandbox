  // The generic dashboard patterns: one boot, a page-wide read budget, paced reads, honest values, batch and
  // visible-only totals, cached queries, per-section loading and keyed regions. ONE source: scripts/gen-dashboard-kit-core.mjs
  // splices this file, unchanged, into dashboard_kit/kit.js (in-app, window.CS and the in-app kit object) and toolkit/cs-live.js
  // (hosted, CS). Each host registers an adapter; the first core on a page wins and a second file adopts it, so a page that
  // loads both has one budget and one cache. Timers and the clock are read at call time, so fake clocks drive every piece.
  // paced, honest, totals, focus and patches are promoted from the context repo's dashboards/_shared and story-journey.
  var csCore = window.__csKitCore || (window.__csKitCore = (function () {
    var core = { version: 4, adapters: {} };
    function now() { return Date.now(); }
    function later(fn, ms) { return window.setTimeout(fn, ms); }
    function unlater(handle) { window.clearTimeout(handle); }
    var versionSerial = 0;
    // A version for content: its JSON, or, when that cannot be made (circular, BigInt), one that is new every time so the row repaints.
    function jsonOr(v) {
      try {
        var text = JSON.stringify(v);
        return text === undefined ? "" : text;
      } catch (e) {
        return "#" + (++versionSerial);
      }
    }
    function stable(v) {
      if (v && typeof v === "object") {
        if (Array.isArray(v)) return "[" + v.map(stable).join(",") + "]";
        return "{" + Object.keys(v).sort().map(function (k) { return JSON.stringify(k) + ":" + stable(v[k]); }).join(",") + "}";
      }
      return JSON.stringify(v === undefined ? null : v);
    }
    function el(tag, cls, text) {
      var node = document.createElement(tag);
      if (cls) node.className = cls;
      if (text != null) node.textContent = text;
      return node;
    }
    var styled = false;
    function style() {
      if (styled || !document.head) return;
      styled = true;
      var s = document.createElement("style");
      s.setAttribute("data-cs-kit-core", "");
      s.textContent = ".cs-section-body{margin-top:8px}.cs-section-skeleton{display:grid;gap:6px}.cs-section-skeleton>span{height:12px;border-radius:4px;background:var(--cs-surface-2,rgba(128,128,128,.2))}" +
        ".cs-section-meta button{margin-left:6px}.cs-section-meta,.cs-section-error{font-size:12px;color:var(--cs-muted,#888);margin-top:6px}.cs-section-error{color:var(--cs-danger,#c0392b)}.cs-value[data-state=waiting],.cs-value[data-state=pending],.cs-value[data-state=loading],.cs-value[data-state=unknown],.cs-value[data-state=error],.cs-value[data-state=stale]{color:var(--cs-muted,#888)}" +
        ".cs-kit-banner{font-size:12px;padding:6px 10px;border-radius:6px;background:var(--cs-surface-2,rgba(128,128,128,.15))}.cs-kit-meter{position:fixed;right:8px;bottom:8px;z-index:2147483000;font:11px/1.4 ui-monospace,monospace;padding:4px 8px;border-radius:6px;background:#000c;color:#fff}";
      document.head.appendChild(s);
    }

    // ── Hosts and K1 boot ──────────────────────────────────────────────────
    // The host is the adapter whose bridge object is present, never a guess from the DOM (script[src], currentScript, file name).
    core.register = function (name, adapter) { adapter.name = name; core.adapters[name] = adapter; };
    function pick() {
      var app = core.adapters.app;
      return app && app.present() ? app : core.adapters.hosted || app;
    }
    core.host = function () { return pick().name; };

    // ── K9 budget: every kit read goes through one page-wide queue ─────────
    // inFlight reads at once; reads in the same slot start at least minGapMs apart (a refresh burst is spaced out); a read that
    // does not answer in timeoutMs (default: the engine's own 900 s backstop, so a slow but legitimate read is never cut off; an ask
    // can pass its own timeoutMs) frees its slot; cancelling a read frees it at once and stops the host retrying it. The Mac answering
    // busy is "busy": pages say "Waiting for the Mac", never an error. Reads merely queued behind a full budget are "waiting".
    var cfg = { inFlight: 2, minGapMs: 2000, bytesPerMinute: 0, timeoutMs: 900000 };
    var running = 0, calls = 0, bytes = 0, queue = [], sizes = [], slotStarts = new Map(), gate = null, meter = null, watchers = [], busyCount = 0;
    var BUSY_CODE = /\b(?:mac_busy|busy|rate_limited)\b/;
    function superseded() { return Object.assign(new Error("superseded"), { superseded: true }); }
    function cancelled() { return Object.assign(new Error("cancelled"), { superseded: true, cancelled: true }); }
    core.isSuperseded = function (e) { return !!(e && e.superseded); };
    // THE code table: every code a read can throw, tagged refusal (the page may not ask this), transient (the Mac, the relay or the wire may
    // answer next time) or other, with its sentence. The hosted codes are the R23 list (LIVE_ERRORS in live_data_authoring.rs: a test fails
    // when a code is missing here); the in-app forms follow it. A "verb" refusal refuses the whole verb for this page, not some ids.
    // A code is its prefix: the wire answers a bare code, the app bridge "code: detail" or free text.
    var GENERIC_ERROR = "Could not read this", REFUSED = "This page can't show this", SILENT = "The Mac didn't answer in time", BUSY_TEXT = "The Mac is busy";
    var CODES = {
      arg_not_allowed: ["refusal", REFUSED], query_not_declared: ["refusal", REFUSED, "verb"], query_invalid: ["refusal", REFUSED],
      access_ended: ["refusal", "Access to this dashboard has ended", "verb"],
      invalid_args: ["refusal", REFUSED], not_approved: ["refusal", REFUSED], not_declared: ["refusal", REFUSED, "verb"],
      undeclared: ["refusal", REFUSED, "verb"], refused: ["refusal", REFUSED],
      mac_offline: ["transient", "The Mac is offline"], mac_busy: ["transient", BUSY_TEXT], busy: ["transient", BUSY_TEXT],
      rate_limited: ["transient", BUSY_TEXT], mac_timeout: ["transient", SILENT], no_answer: ["transient", SILENT], timeout: ["transient", SILENT],
      answer_failed: ["other", "The Mac couldn't answer"], answer_too_large: ["other", "Too much to show at once"],
      stale_cursor: ["other", "The list changed while loading"], bad_request: ["other", GENERIC_ERROR],
      post_to_own_window: ["other", GENERIC_ERROR], not_owner: ["other", GENERIC_ERROR], needs_gesture: ["other", GENERIC_ERROR],
      failed: ["other", "The Mac couldn't read this"], omitted: ["other", GENERIC_ERROR], no_totals: ["other", GENERIC_ERROR],
    };
    function lookup(e) {
      var code = typeof e === "string" ? e : e instanceof Error ? e.message : "";
      var m = /^([a-z_]+)(?::|$)/.exec(code), hit = m && Object.prototype.hasOwnProperty.call(CODES, m[1]) ? CODES[m[1]] : null;
      if (hit) return { kind: hit[0], text: hit[1], verb: hit[2] === "verb" };
      if (/is not allowlisted$/.test(code)) return { kind: "refusal", text: REFUSED, verb: true };
      if (/^storyTotals: /.test(code)) return { kind: "refusal", text: REFUSED, verb: false };
      return { kind: "other", text: GENERIC_ERROR, verb: false };
    }
    // A read's error as a sentence, never a raw code or [object Object]: a known code gets its own sentence; anything else (and any
    // value that is neither an Error nor a string) is "Could not read this".
    core.errorText = function (e) { return lookup(e).text; };
    core.errorKind = function (e) { return lookup(e).kind; };
    core.isRefusal = function (e) { return lookup(e).kind === "refusal"; };
    core.isVerbRefusal = function (e) { return lookup(e).verb; };
    core.errorCodes = function () {
      var out = {};
      Object.keys(CODES).forEach(function (code) { out[code] = CODES[code][0]; });
      return out;
    };
    core.isBusy = function (e) { return BUSY_CODE.test(String((e && e.message) || e)); };
    function waiting() { return busyCount > 0 || queue.some(function (q) { return !q.held; }); }
    core.waiting = waiting;
    // The hosts say when a read is being asked again because the Mac or the relay answered busy.
    // With a signal (one budgeted read's), the note is idempotent: cancelling or timing out the read clears it at once.
    core.noteBusy = function (on, signal) {
      if (signal) {
        if (!!signal.busy === on) return;
        signal.busy = on;
      }
      busyCount = Math.max(0, busyCount + (on ? 1 : -1));
      changed();
    };
    core.busy = function () { return busyCount > 0; };
    function stats() { return { calls: calls, bytes: bytes, inFlight: running, queued: queue.length, waiting: waiting(), busy: busyCount > 0 }; }
    function changed() {
      if (!meter && /[?&]csmeter=1(&|$)/.test(window.location.search) && document.body) {
        style();
        meter = el("div", "cs-kit-meter");
        document.body.appendChild(meter);
      }
      var s = stats();
      if (meter) meter.textContent = "reads " + s.calls + " · " + s.bytes + " B · in flight " + s.inFlight + " · queued " + s.queued;
      watchers.slice().forEach(function (cb) { cb(s); });
    }
    function windowBytes() {
      var cut = now() - 60000;
      sizes = sizes.filter(function (r) { return r.at > cut; });
      return sizes.reduce(function (sum, r) { return sum + r.n; }, 0);
    }
    // A watcher (or anything it calls) that pumps while a pump runs is deferred to the end of that pump, never recursed into.
    var pumping = false, pumpAgain = false;
    function pump() {
      if (pumping) { pumpAgain = true; return; }
      pumping = true;
      try {
        for (var rounds = 0; rounds < 5; rounds++) {
          pumpAgain = false;
          pumpOnce();
          if (!pumpAgain) break;
        }
      } finally {
        pumping = false;
      }
    }
    function pumpOnce() {
      var wake = Infinity;
      for (var i = 0; i < queue.length && running < cfg.inFlight;) {
        var item = queue[i];
        if (item.stale && item.stale()) { queue.splice(i, 1); item.fail(superseded()); continue; }
        var gap = item.slot && slotStarts.has(item.slot) ? slotStarts.get(item.slot) + cfg.minGapMs - now() : 0;
        var bytesWait = cfg.bytesPerMinute && windowBytes() >= cfg.bytesPerMinute ? 1000 : 0;
        item.held = gap > 0 && !bytesWait;
        if (gap > 0 || bytesWait) { wake = Math.min(wake, Math.max(gap, bytesWait)); i++; continue; }
        queue.splice(i, 1);
        start(item);
      }
      unlater(gate);
      gate = wake < Infinity ? later(function () { gate = null; pump(); }, wake) : null;
      changed();
    }
    function start(item) {
      running++;
      calls++;
      if (item.slot) {
        slotStarts.set(item.slot, now());
        if (slotStarts.size > 500) slotStarts.forEach(function (t, k) { if (now() - t > cfg.minGapMs) slotStarts.delete(k); });
      }
      var done = false, timer = later(function () { settle(function () { item.fail(Object.assign(new Error("timeout"), { timeout: true })); }, true); }, item.timeoutMs || cfg.timeoutMs);
      // `stop`: the read is abandoned (cancelled or timed out): the host must not ask it again, and its busy note is cleared at once.
      function settle(finish, stop) {
        if (done) return;
        done = true;
        unlater(timer);
        running--;
        item.cancel = function () {};
        if (stop) {
          item.signal.cancelled = true;
          core.noteBusy(false, item.signal);
        }
        finish();
        pump();
      }
      item.cancel = function () { settle(function () { item.fail(cancelled()); }, true); };
      item.run(item.signal).then(function (m) {
        settle(function () {
          if (cfg.bytesPerMinute || meter) {
            var n = 0;
            try { n = JSON.stringify(m && m.answer !== undefined ? m.answer : m).length; } catch (e) { /* not countable */ }
            bytes += n;
            sizes.push({ at: now(), n: n });
          }
          item.ok(m);
        });
      }, function (e) { settle(function () { item.fail(e); }); });
    }
    // opts.slot: a newer ask in the same slot drops an older one still queued. opts.stale(): true drops the ask at its turn.
    // opts.timeoutMs: this ask's own timeout. opts.box: {cancel} is filled in; box.cancel() drops the ask if queued, or frees its
    // slot if it is running. run(signal) gets {cancelled, busy}: a host retrying the read stops when signal.cancelled is set.
    function schedule(run, opts) {
      opts = opts || {};
      if (opts.slot) {
        queue = queue.filter(function (q) {
          if (q.slot !== opts.slot) return true;
          q.fail(superseded());
          return false;
        });
      }
      return new Promise(function (ok, fail) {
        var item = { run: run, ok: ok, fail: fail, slot: opts.slot, stale: opts.stale, timeoutMs: opts.timeoutMs, held: false, signal: { cancelled: false, busy: false } };
        item.cancel = function () {
          var i = queue.indexOf(item);
          if (i < 0) return;
          queue.splice(i, 1);
          fail(cancelled());
          changed();
        };
        if (opts.box) opts.box.cancel = function () { item.cancel(); };
        queue.push(item);
        pump();
      });
    }
    core.budget = function (opts) {
      // A bad value (zero, negative, NaN) keeps the current setting: inFlight is at least 1 (the default is 2).
      // Only a changed setting pumps: budget() with no or unchanged options has no side effect, so a watcher may call it freely.
      var before = JSON.stringify(cfg);
      if (opts) Object.keys(opts).forEach(function (k) {
        var n = Number(opts[k]);
        if (k === "inFlight" && n >= 1) cfg.inFlight = Math.floor(n);
        else if (k === "timeoutMs" && n > 0) cfg.timeoutMs = n;
        else if ((k === "minGapMs" || k === "bytesPerMinute") && n >= 0) cfg[k] = n;
      });
      if (JSON.stringify(cfg) !== before) pump();
      return {
        config: function () { return Object.assign({}, cfg); },
        stats: stats,
        // "idle" or "waiting" (the Mac answered busy, or reads are queued behind the budget).
        state: function () { return waiting() ? "waiting" : "idle"; },
        onChange: function (cb) {
          watchers.push(cb);
          return function () { watchers = watchers.filter(function (w) { return w !== cb; }); };
        },
      };
    };
    // Cancel handles of the asks a caller made, kept in the caller's list while they are in flight: an ask removes its box when it settles.
    core.tracked = function (boxes, ask) {
      var box = {};
      boxes.push(box);
      var done = function () { var i = boxes.indexOf(box); if (i >= 0) boxes.splice(i, 1); };
      var promise = ask(box);
      promise.then(done, done);
      return promise;
    };
    // The answer message ({answer, source, answered_at}) of one read. The adapter's ask gets opts too (opts.paging: no relay retry).
    core.ask = function (name, args, opts) {
      return schedule(function (signal) { return pick().ask(name, args || {}, Object.assign({}, opts, { signal: signal })); }, opts);
    };
    core.read = function (name, args, opts) { return core.ask(name, args, opts).then(function (m) { return m.answer; }); };

    // ── K2 paced reads (paced.js) ──────────────────────────────────────────
    var PACED_STALE = /stale_cursor/;
    // One "the Mac is busy" signal per paced owner, however many times its read is asked again.
    function busyNote() {
      var on = false;
      return function (v) { if (v !== on) { on = v; core.noteBusy(v); } };
    }
    function createPaced(o) {
      var run = o.run, minGapMs = o.minGapMs == null ? 2000 : o.minGapMs, ttlMs = o.ttlMs || 0, onBusy = o.onBusy || function () {};
      var retryDelays = o.retryDelays || [1000, 2000, 4000, 8000], staleTries = o.staleTries == null ? 3 : o.staleTries;
      var stopped = false, gapTimer = null, busyTimer = null, wakeBusy = null, going = false, wanted = false, lastStart = -Infinity, lastOk = -Infinity, waiters = [];
      function pause(ms) {
        return new Promise(function (resolve) { wakeBusy = resolve; busyTimer = later(function () { busyTimer = null; wakeBusy = null; resolve(); }, ms); });
      }
      async function attempt() {
        var busyTries = 0, staleCount = 0;
        while (!stopped) {
          try {
            var out = await run();
            onBusy(false);
            lastOk = now();
            return out;
          } catch (error) {
            var code = String((error && error.message) || error);
            // A dead cursor restarts the read from the top, without a word to the page.
            if (PACED_STALE.test(code) && staleCount++ < staleTries) continue;
            if (BUSY_CODE.test(code)) { onBusy(true); await pause(retryDelays[Math.min(busyTries++, retryDelays.length - 1)]); continue; }
            onBusy(false);
            throw error;
          }
        }
      }
      function begin() {
        gapTimer = null;
        if (stopped || going) return;
        wanted = false;
        lastStart = now();
        going = true;
        var group = waiters;
        waiters = [];
        attempt().then(function (v) { group.forEach(function (w) { w.ok(v); }); }, function (e) { group.forEach(function (w) { w.fail(e); }); })
          .finally(function () { going = false; if (wanted && !stopped) plan(); });
      }
      function plan() {
        if (gapTimer || going || stopped) return;
        var wait = Math.max(0, lastStart + minGapMs - now());
        if (wait === 0) begin(); else gapTimer = later(begin, wait);
      }
      // Requests made while a run is going, or inside the gap, share the next run. {force:false} with a ttl answers at once
      // when the last good run is younger than ttlMs and nothing else is waiting.
      function request(r) {
        var force = !!(r && r.force);
        if (stopped) return Promise.resolve();
        if (!force && ttlMs > 0 && now() - lastOk < ttlMs && !wanted && !going) return Promise.resolve();
        wanted = true;
        var promise = new Promise(function (ok, fail) { waiters.push({ ok: ok, fail: fail }); });
        plan();
        return promise;
      }
      function stop() {
        stopped = true;
        onBusy(false);
        unlater(gapTimer);
        unlater(busyTimer);
        gapTimer = busyTimer = null;
        if (wakeBusy) wakeBusy();
        wakeBusy = null;
        var list = waiters;
        waiters = [];
        list.forEach(function (w) { w.ok(); });
      }
      return { request: request, stop: stop, get running() { return going; } };
    }
    // paced(query, {key, limit, inFlight, ttlMs, minGapMs, margin}): query is a read name (budgeted) or (args) => Promise.
    // request(args) runs now; observe(el, args) runs when the element nears the viewport, at most `inFlight` (3) at once.
    core.paced = function (query, o) {
      o = o || {};
      var owners = new Map(), observed = new Map(), waiting = [], going = 0, cap = o.inFlight || 3, observer = null, stopped = false, asked = new Set(), boxes = [];
      function owner(args) {
        var k = stable(args);
        if (!owners.has(k)) {
          var runQuery = typeof query === "function" ? function () { return query(args); }
            : function () {
              return core.tracked(boxes, function (box) { return core.read(query, Object.assign({}, args, o.limit ? { limit: o.limit } : {}), { slot: (o.key || query) + k, box: box }); });
            };
          owners.set(k, createPaced({ run: runQuery, minGapMs: o.minGapMs, ttlMs: o.ttlMs, onBusy: busyNote() }));
        }
        return owners.get(k);
      }
      function pump() {
        while (going < cap && waiting.length) {
          var args = waiting.shift();
          going++;
          owner(args).request().catch(function () {}).finally(function () { going--; pump(); });
        }
      }
      // An element's args are read once, however often it scrolls back into view; refresh() reads them again.
      // Nodes that left the document are no longer watched. A node built before it is attached is kept: only a node once seen
      // connected and now detached is forgotten.
      var wasConnected = new WeakSet();
      function forgetDetached() {
        observed.forEach(function (args, node) {
          if (node.isConnected) { wasConnected.add(node); return; }
          if (!wasConnected.has(node)) return;
          if (observer) observer.unobserve(node);
          observed.delete(node);
        });
      }
      function enqueue(args) {
        var k = stable(args);
        if (asked.has(k)) return;
        asked.add(k);
        if (!waiting.some(function (a) { return stable(a) === k; })) waiting.push(args);
        pump();
      }
      function watch() {
        if (observer || typeof IntersectionObserver === "undefined") return observer;
        observer = new IntersectionObserver(function (entries) {
          forgetDetached();
          entries.forEach(function (e) { if (e.isIntersecting && observed.has(e.target)) enqueue(observed.get(e.target)); });
        }, { rootMargin: (o.margin == null ? 200 : o.margin) + "px" });
        return observer;
      }
      return {
        request: function (args, r) { return owner(args || {}).request(r); },
        observe: function (node, args) {
          if (stopped) return;
          forgetDetached();
          observed.set(node, args || {});
          if (node.isConnected) wasConnected.add(node);
          if (watch()) observer.observe(node); else enqueue(args || {});
        },
        // The next run of every owner that has run or is waiting, shared by requests already queued.
        refresh: function () { asked.clear(); owners.forEach(function (p) { p.request({ force: true }).catch(function () {}); }); },
        stop: function () {
          stopped = true;
          if (observer) observer.disconnect();
          owners.forEach(function (p) { p.stop(); });
          boxes.splice(0).forEach(function (b) { if (b.cancel) b.cancel(); });
          observed.clear();
          waiting = [];
        },
        get running() { return going; },
      };
    };

    // ── K3 honest states (honest.js, freshness.js) ─────────────────────────
    var LOADING = "Loading…", PENDING = "Counting…", WAITING = "Waiting for the Mac", UNKNOWN = "Unknown", NOT_LOADED = "Not loaded";
    function sentence(reason) { return reason ? String(reason)[0].toUpperCase() + String(reason).slice(1) : ""; }
    function unknownLabel(reason) { return reason ? UNKNOWN + " · " + sentence(reason) : UNKNOWN; }
    function reasonAt(reasons, index) { return Array.isArray(reasons) && Number.isInteger(index) && typeof reasons[index] === "string" ? reasons[index] : ""; }
    function distinctReasons(texts) {
      return Array.from(new Set(texts.filter(function (t) { return typeof t === "string" && t; })));
    }
    function dataAge(answeredAt) { return answeredAt == null ? null : Math.max(0, Math.floor((now() - answeredAt) / 60000)); }
    function freshnessNote(o) {
      var age = o.dataAge == null ? "the last saved data" : "data from " + (o.dataAge === 0 ? "less than a minute" : o.dataAge + " minute" + (o.dataAge === 1 ? "" : "s")) + " ago";
      if (o.offline) return o.hasData ? "The owner's Mac is offline. Showing " + age + ". The page updates when it's back." : "The owner's Mac is offline. The page updates when it's back.";
      if (o.stale) return o.dataAge == null ? "Showing cached data. The page updates when fresh data is available." : "Showing cached " + age + ". The page updates when fresh data is available.";
      return null;
    }
    // A figure is exactly one of: known (a 0 stays a 0), Waiting for the Mac (the Mac is busy: waiting, or an error that says busy),
    // Counting… (pending), Loading…, Unknown · reason, Not loaded (read failed).
    // A missing value is never turned into 0, a bare dash or "at least 0".
    core.valueState = function (v, o) {
      o = o || {};
      if (v !== null && v !== undefined && !(typeof v === "number" && !isFinite(v))) {
        var text = o.format ? o.format(v) : String(v);
        var late = o.stale || o.offline;
        return { state: late ? "stale" : "known", text: late && o.ageMs != null ? text + " · " + Math.round(o.ageMs / 60000) + " min ago" : text };
      }
      if (o.waiting || (o.error && core.isBusy(o.error))) return { state: "waiting", text: WAITING };
      if (o.pending) return { state: "pending", text: PENDING };
      if (o.loading) return { state: "loading", text: LOADING };
      if (o.error) {
        var why = o.error === true ? GENERIC_ERROR : core.errorText(o.error);
        return { state: "error", text: why === GENERIC_ERROR ? NOT_LOADED : NOT_LOADED + " · " + why };
      }
      return { state: "unknown", text: unknownLabel(o.unavailable) };
    };
    core.value = function (v, o) {
      var s = core.valueState(v, o);
      var node = el("span", "cs-value", s.text);
      node.setAttribute("data-state", s.state);
      if (s.state === "error" && o && o.onRetry) {
        var b = el("button", "cs-btn is-sm", "Retry");
        b.type = "button";
        b.addEventListener("click", o.onRetry);
        node.append(" ", b);
      }
      style();
      return node;
    };
    core.stateBanner = function (o) {
      o = o || {};
      var text = freshnessNote({ offline: o.offline, stale: o.stale, dataAge: dataAge(o.asOf), hasData: o.hasData !== false });
      if (!text) return null;
      var node = el("div", "cs-kit-banner", text);
      node.setAttribute("role", "status");
      style();
      return node;
    };
    // "Totals loading · 120 of 770 stories": a partial total is progress, never a figure.
    core.partial = function (known, total, noun) { return known >= total ? "" : "Totals loading · " + known + " of " + total + " " + (noun || "stories"); };

    // ── K4 batch totals (totals.js) ────────────────────────────────────────
    var TOTALS_FOLLOW_DELAYS = [2000, 3000, 5000, 8000], TOTALS_FOLLOW_LIMIT = 6;
    // What a storyTotals answer says, per story: ready (the engine's totals, nulls kept, each reason resolved to text), pending
    // (not indexed yet) or error. Pacing, busy backoff and debounce belong to paced and the budget; this only says what to ask next.
    function createTotalsReader(o) {
      var read = o.read, getIds = o.getIds, retryable = o.retryable, name = o.name || "storyTotals", warnedPin = false, everReady = false, dirty = new Map(), dirtyGen = 0, askedDirty = new Map(), verbRefused = false, byId = new Map(), indexing = null, answeredAt = null, source = null, loaded = false, follows = 0;
      function resolved(story, list) {
        var out = {};
        Object.keys(story.unavailable || {}).forEach(function (field) {
          var text = reasonAt(list, story.unavailable[field]);
          if (text) out[field] = text;
        });
        return out;
      }
      // A failure keeps a story's last totals; it is asked again only when it is one the Mac or the wire clears by itself.
      function failed(before, had, code) {
        var again = !retryable || retryable({ kind: "error", error: code });
        return had ? Object.assign({}, before, { refreshing: again, kept: true, error: code }) : { kind: "error", error: code };
      }
      // A story answered again no longer carries the failure that kept its old totals.
      function withoutFailure(entry) {
        var copy = Object.assign({}, entry);
        delete copy.kept;
        delete copy.error;
        return copy;
      }
      function facet(story, key) { return key in story ? (Number.isFinite(story[key]) ? story[key] : null) : undefined; }
      function apply(answer, asked, gens) {
        var list = Array.isArray(answer && answer.reasons) ? answer.reasons : [];
        asked.forEach(function (id) {
          // A successful answer settles only the event this ask was made for: one that arrived during the read stays dirty.
          if (dirty.has(id) && dirty.get(id) === gens.get(id)) dirty.delete(id);
          var story = answer && answer.stories && answer.stories[id], before = byId.get(id), had = !!before && before.kind === "ready";
          if (!story || typeof story !== "object" || Array.isArray(story)) byId.set(id, failed(before, had, "omitted"));
          else if (story.pending === true) byId.set(id, had ? Object.assign(withoutFailure(before), { refreshing: true }) : { kind: "pending" });
          else if (story.error) byId.set(id, failed(before, had, String(story.error)));
          else if (!story.totals || typeof story.totals !== "object") byId.set(id, failed(before, had, "no_totals"));
          // The totals are story_changes' totals, field for field: a null stays null, and nothing here turns it into a 0.
          else byId.set(id, {
            kind: "ready", totals: Object.assign({}, story.totals), unavailable: resolved(story, list),
            computedAtMs: story.computedAtMs == null ? null : story.computedAtMs, refreshing: story.refreshing === true,
            mergedAt: "mergedAt" in story ? (Number.isFinite(story.mergedAt) ? story.mergedAt : null) : undefined,
            mergedAtReason: typeof story.mergedAtReason === "string" ? story.mergedAtReason : "",
            // The activity facets pass through as mergedAt does: a finite number or null when the entry names the key, never a made-up zero.
            firstActivityAt: facet(story, "firstActivityAt"), lastActivityAt: facet(story, "lastActivityAt"), openSessions: facet(story, "openSessions"),
            activityReason: typeof story.activityReason === "string" ? story.activityReason : undefined,
            prs: Array.isArray(story.prs) ? story.prs.filter(function (p) { return p && Number.isFinite(p.number); }).map(function (p) { return { number: p.number, merged: Number.isFinite(p.merged) ? p.merged : null }; }) : undefined,
            prsTotal: Number.isFinite(story.prsTotal) ? story.prsTotal : undefined,
            runtimes: Array.isArray(story.runtimes) ? story.runtimes.filter(function (k) { return typeof k === "string"; }) : undefined,
          });
        });
        if (answer && answer.indexing && typeof answer.indexing === "object") indexing = answer.indexing;
        if (answer && Number.isFinite(Number(answer.answered_at))) answeredAt = Number(answer.answered_at);
        source = answer && answer.source != null ? answer.source : source;
        // An answer came back, so the verb and the pin accepted this ask (ready, pending or a per-story error alike). From now on no
        // refusal is ever read as the verb or the pin being wrong for the page.
        everReady = true;
      }
      // Not settled: unseen, pending, errored, or ready but recomputing. Only ids still in the list count.
      function needs(id) {
        var s = byId.get(id);
        if (!s) return true;
        if (isRefused(s)) return false;
        if (dirty.has(id)) return true;
        if (s.kind === "ready") return s.refreshing;
        return s.kind === "pending" || !retryable || retryable(s);
      }
      function unsettled() { return getIds().filter(needs); }
      // Never attempted (an ask that threw still counts as attempted: the follow-up timer retries it), or named by a data-change event not asked yet.
      var attempted = new Set();
      // A dirty id whose event was already asked (and failed) is the follow-up timers' to retry, so a show does not ask it again.
      function unseen(id) { return !attempted.has(id) || (dirty.has(id) && askedDirty.get(id) !== dirty.get(id)); }
      // Already showing values and only recomputing on the Mac: asked on a long interval, never on the fast follow-ups.
      function slow(id) {
        var s = byId.get(id);
        return !!s && s.kind === "ready" && s.refreshing === true && !s.error && !dirty.has(id);
      }
      function fast(id) { return needs(id) && !slow(id); }
      // pull({all, restart}): every id (open, refresh, live event), or only the unsettled ones (restart: start the backoff over). One read either
      // way. pull({slow}) asks only the ready-but-recomputing ones.
      // A refused story (the page may not ask it) is left out of every automatic pull; only pull({refused: true}) asks it again.
      function isRefused(s) { return !!s && typeof s.error === "string" && core.isRefusal(s.error); }
      function markRefused(id) {
        var before = byId.get(id);
        byId.set(id, failed(before, !!before && before.kind === "ready", "refused"));
      }
      async function pull(p) {
        var refusedToo = !!(p && p.refused);
        if (refusedToo) verbRefused = false;
        var all = !p || (p.all !== false && !p.slow);
        var asked = p && p.ids ? p.ids.filter(function (id) { return !isRefused(byId.get(id)); })
          : p && p.slow ? getIds().filter(slow)
          : all ? getIds().filter(function (id) { return refusedToo || !isRefused(byId.get(id)); }) : unsettled();
        if (!asked.length) return;
        // Written before the read, so a failed ask is still an attempt; a dirty id stays dirty until an answer arrives.
        // gens: the event each dirty id carries now, kept by THIS ask, so an answer settles only what it was asked for.
        var gens = new Map();
        asked.forEach(function (id) {
          attempted.add(id);
          if (dirty.has(id)) { gens.set(id, dirty.get(id)); askedDirty.set(id, dirty.get(id)); } else askedDirty.delete(id);
        });
        // A new ask (open, refresh, scroll) starts the backoff over; only a follow-up counts toward it. Naming ids touches neither.
        if (p && (p.ids || p.slow)) { /* exactly these ids */ } else if (all || (p && p.restart)) follows = 0; else follows++;
        // The verb itself was refused for this page: nothing is asked again until pull({refused: true}).
        if (verbRefused) { asked.forEach(markRefused); loaded = true; return; }
        await readIds(asked, gens);
        loaded = true;
      }
      // One ask: "ok", "refused" (a refusal code: some of these ids may not be asked), "pin" (arg_not_allowed: a missing or wrong pin, which
      // refuses every id alike) or "verb" (the whole verb is refused for this page, which one ask is enough to know). Any other thrown
      // error is transient: it is thrown, whole, for the caller's backoff.
      async function attempt(ids, gens) {
        try {
          apply(await read(ids), ids, gens);
          return "ok";
        } catch (e) {
          if (core.isSuperseded(e) || !core.isRefusal(e)) throw e;
          if (core.isVerbRefusal(e)) return "verb";
          return /^arg_not_allowed(?::|$)/.test(String((e && e.message) || e)) ? "pin" : "refused";
        }
      }
      // The rule: an id is marked refused only when its own ask was refused, or the server said the whole verb is refused (a verb code).
      // The one inference, the unpinned verdict below, is only ever made before this reader has had any answer. The page-wide flag
      // (nothing is asked again) is set only then too; after that a verb code marks only its own batch, and later pulls ask normally.
      function refuseVerb(ids) {
        if (!everReady) verbRefused = true;
        ids.forEach(markRefused);
      }
      // ids were refused as one ask: halve them and ask each half once through the budget, and keep halving each half that is refused,
      // down to single ids. A refusal is never inferred for ids that were not refused themselves.
      async function isolate(ids, gens) {
        if (ids.length === 1) { markRefused(ids[0]); return; }
        var mid = Math.ceil(ids.length / 2), halves = [ids.slice(0, mid), ids.slice(mid)];
        var results = await Promise.all(halves.map(function (h) { return attempt(h, gens); }));
        // Past the first round some id may already have been accepted, so no half is read as the verb being refused: it is halved on.
        await Promise.all(halves.filter(function (h, i) { return results[i] !== "ok"; }).map(function (h) { return isolate(h, gens); }));
      }
      // ids of unknown standing (some probes were refused, so the rest may be fine): one id is asked, more are halved and asked.
      async function sift(ids, gens) {
        if (ids.length === 1) { if ((await attempt(ids, gens)) !== "ok") markRefused(ids[0]); return; }
        await isolate(ids, gens);
      }
      async function readIds(ids, gens) {
        var result = await attempt(ids, gens);
        if (result === "verb") { refuseVerb(ids); return; }
        if (result === "ok") return;
        if (result === "pin" && ids.length > 1) {
          // arg_not_allowed on many ids: probe three (first, middle, last; all of them when there are three or fewer) in one budgeted round.
          // LEAD DECISION (accepted residual risk): on a reader that has never had an answer, a batch whose first, middle and last ids are
          // ALL refused this way is treated as unpinned (every id refused, one console warning) until refresh({refused: true}). Once the
          // reader has had any answer, nothing is inferred: refused probes are marked and the rest is sifted, id by id if need be.
          var picks = [ids[0], ids[Math.floor(ids.length / 2)], ids[ids.length - 1]].filter(function (id, i, all) { return all.indexOf(id) === i; });
          var probes = await Promise.all(picks.map(function (id) { return attempt([id], gens); }));
          var accepted = probes.some(function (r) { return r === "ok"; });
          if (!accepted && !everReady && probes.every(function (r) { return r === "pin" || r === "verb"; })) {
            if (!warnedPin) {
              warnedPin = true;
              console.warn(name + " refused for every probed id: check the page's storyIds pin");
            }
            refuseVerb(ids);
            return;
          }
          picks.forEach(function (id, i) { if (probes[i] !== "ok") markRefused(id); });
          var left = ids.filter(function (id) { return picks.indexOf(id) < 0; });
          if (!left.length) return;
          // Nothing answered yet on this reader: the rest is read fresh. Otherwise the rest is sifted: never read as a whole, never given
          // a verdict, and a single remaining id is asked rather than deduced.
          if (!accepted && !everReady) await readIds(left, gens); else await sift(left, gens);
          return;
        }
        await isolate(ids, gens);
      }
      // The delay before the next follow-up, or null when every story is settled or the limit is reached for ones that may not settle.
      function nextDelay() {
        var open = unsettled();
        if (!open.length) return null;
        var onlyPending = open.every(function (id) { return ((byId.get(id) || {}).kind || "pending") === "pending"; });
        if (!onlyPending && follows >= TOTALS_FOLLOW_LIMIT) return null;
        return TOTALS_FOLLOW_DELAYS[Math.min(follows, TOTALS_FOLLOW_DELAYS.length - 1)];
      }
      return {
        pull: pull, nextDelay: nextDelay, unsettled: unsettled, needs: needs,
        unseenIds: function () { return getIds().filter(unseen); }, isFast: fast, fastIds: function () { return getIds().filter(fast); }, slowIds: function () { return getIds().filter(slow); },
        get: function (id) { return byId.get(id); },
        get loaded() { return loaded; }, get indexing() { return indexing; }, get answeredAt() { return answeredAt; }, get source() { return source; },
        known: function (ids) { return ids.filter(function (id) { return (byId.get(id) || {}).kind === "ready"; }).length; },
        reasons: function (ids) { return distinctReasons(ids.flatMap(function (id) { return Object.values((byId.get(id) || {}).unavailable || {}); })); },
        // A data-change event named these ids: they are asked again when visible (or when they scroll in), and nothing else is.
        // Only ids this reader has answered for can go stale, and the set is capped: an id never seen is dropped.
        markDirty: function (ids) { ids.forEach(function (id) { if (byId.has(id) && (dirty.has(id) || dirty.size < 1000)) dirty.set(id, ++dirtyGen); }); },
        forget: function () { byId.clear(); dirty.clear(); askedDirty.clear(); attempted.clear(); verbRefused = false; everReady = false; indexing = null; loaded = false; follows = 0; },
      };
    }
    function totalsRead(query, slot, boxes) {
      // `boxes` (a list owned by the caller) collects each ask's cancel handle, so stopping the caller frees its budget slots.
      var own = boxes || [];
      return function (part) {
        if (query !== "storyTotals") return core.tracked(own, function (box) { return core.read(query, { storyIds: part }, { slot: slot, box: box }); });
        // The host sizes and merges its own asks; each one goes through the budget.
        return pick().storyTotals(part, function (ids) { return core.tracked(own, function (box) { return core.ask("storyTotals", { storyIds: ids }, { slot: slot && slot + stable(ids), box: box }); }); });
      };
    }
    // batchTotals(ids | () => ids, {query: 'storyTotals'}): a reader. pull() makes ONE storyTotals call per <= 500 ids.
    core.batchTotals = function (ids, o) {
      o = o || {};
      return createTotalsReader({ read: o.read || totalsRead(o.query || "storyTotals", o.slot, o.boxes), getIds: typeof ids === "function" ? ids : function () { return ids; }, retryable: o.retryable, name: o.query || "storyTotals" });
    };
    // K4+: totals for the rows on screen only. Visible ids are batched and debounced, each id cached with its computedAtMs;
    // pending ids are asked again with backoff while visible, refreshing ones on a long interval; an id that was never in view is never
    // asked, and a show asks nothing for an id already asked.
    var FOLLOW_MS = [2000, 4000, 8000, 16000], SLOW_MS = [60000, 120000, 240000];
    var totalsHandles = new Set(), totalsSerial = 0;
    core.visibleTotals = function (container, o) {
      o = o || {};
      var attr = o.idAttr || "data-id", debounce = o.debounceMs == null ? 150 : o.debounceMs, visible = new Set(), timer = null, follow = null, stopped = false, tries = 0, io = null, mo = null, named = new Set(), namedTimer = null, chasing = new Set(), chaseTimer = null, chaseTries = 0, slowTimer = null, slowTries = 0, boxes = [], watched = new Set();
      // A story the engine refused (a pin rejection, an unknown id) is not asked again; only busy and transient failures are.
      var reader = core.batchTotals(function () { return Array.from(visible); }, { query: o.query, slot: "visibleTotals:" + (o.slot || attr) + "#" + (++totalsSerial), retryable: function (entry) { return core.errorKind(String(entry.error)) === "transient"; }, boxes: boxes });
      function idOf(row) { return row.getAttribute(attr); }
      var pulls = 0;
      // p: {all, restart, refused}, {ids, restart} or {slow}.
      async function pull(p) {
        if (stopped || !visible.size) return;
        if (p.restart) tries = 0;
        pulls++;
        try {
          await reader.pull(p);
        } catch (e) {
          // A superseded ask is not an answer, but the follow-up it was part of still has to be armed.
          if (!core.isSuperseded(e) && o.onError) o.onError(e);
        } finally {
          pulls--;
        }
        if (stopped) return;
        if (o.onChange) o.onChange(handle);
        arm();
      }
      // A named id still pending or failed belongs to its own chase backoff, so the list's follow-up leaves it out.
      function ownFast() { return reader.fastIds().filter(function (id) { return !chasing.has(id); }); }
      // Pending and retryable-error ids are asked again while visible, after 2, 4, 8 then every 16 s, for as long as they stay on screen.
      // Rows that already show values and are only recomputing (refreshing) are asked at most every 60 s, backing off to 240 s.
      function arm() {
        unlater(follow);
        follow = null;
        armSlow();
        if (stopped || !visible.size || !ownFast().length) { tries = 0; return; }
        // Follow-ups pause while the page is hidden; showing it only starts the timers again: no read fires on return.
        if (document.hidden) return;
        follow = later(function () {
          follow = null;
          if (document.hidden || pulls) return;
          // A try is counted only when an ask is made; rows that left the screen meanwhile start the backoff over.
          var own = ownFast();
          if (!own.length) { tries = 0; return; }
          tries++;
          pull({ ids: own });
        }, FOLLOW_MS[Math.min(tries, FOLLOW_MS.length - 1)]);
      }
      // An armed slow timer is never pushed out; hiding drops it, so a show waits a whole fresh interval.
      function armSlow() {
        if (stopped || document.hidden || !visible.size || !reader.slowIds().length) { unlater(slowTimer); slowTimer = null; slowTries = 0; return; }
        if (slowTimer) return;
        slowTimer = later(function () {
          slowTimer = null;
          if (document.hidden) return;
          if (pulls || !reader.slowIds().length) { armSlow(); return; }
          slowTries = Math.min(slowTries + 1, SLOW_MS.length - 1);
          pull({ all: false, slow: true });
        }, SLOW_MS[slowTries]);
      }
      function onShown() {
        if (stopped) return;
        if (document.hidden) { unlater(slowTimer); slowTimer = null; return; }
        if (!follow) arm();
        if (!chaseTimer && chasing.size) chase();
        if (!namedTimer && named.size) namedTimer = later(flushNamed, debounce);
      }
      document.addEventListener("visibilitychange", onShown);
      // Rows coming into view are asked only if never attempted (or named while off screen); the others belong to the follow-up timers,
      // and ids a live event named while visible belong to flushNamed, so a show, whose observer entries land on every row, makes no ask.
      function plan() {
        unlater(timer);
        timer = later(function () {
          timer = null;
          var fresh = reader.unseenIds().filter(function (id) { return !named.has(id); });
          if (fresh.length) pull({ ids: fresh, restart: true });
        }, debounce);
      }
      // New rows are watched as they arrive; without an IntersectionObserver every row counts as visible.
      function sweep() {
        var present = new Set();
        container.querySelectorAll(o.rowSelector).forEach(function (row) {
          var id = idOf(row);
          if (id) present.add(id);
          if (row.__csTotals) return;
          row.__csTotals = true;
          watched.add(row);
          if (io) io.observe(row); else if (id) visible.add(id);
        });
        // Rows that left the container are no longer watched.
        watched.forEach(function (row) { if (!container.contains(row)) { if (io) io.unobserve(row); watched.delete(row); row.__csTotals = false; } });
        visible.forEach(function (id) { if (!present.has(id)) visible.delete(id); });
        if (!io) plan();
      }
      if (typeof IntersectionObserver !== "undefined") {
        io = new IntersectionObserver(function (entries) {
          entries.forEach(function (e) {
            var id = idOf(e.target);
            if (!id) return;
            if (e.isIntersecting) visible.add(id); else visible.delete(id);
          });
          plan();
          // A pending row that scrolls back in is picked up by the follow-up timer, without an ask of its own.
          if (!follow) arm();
        }, { root: o.root || null });
      }
      if (typeof MutationObserver !== "undefined") { mo = new MutationObserver(sweep); mo.observe(container, { childList: true, subtree: true }); }
      sweep();
      // Named ids still pending or failed in transit are asked again, only they, after 2, 4, 8, then every 16 s while visible. A named id
      // that answers refreshing already shows values, so it leaves this backoff for the slow interval. This backoff is theirs alone: it
      // never touches the list's own follow-ups, nor theirs.
      function chase() {
        chasing.forEach(function (id) { if (!visible.has(id) || !reader.isFast(id)) chasing.delete(id); });
        if (stopped || !chasing.size) { unlater(chaseTimer); chaseTimer = null; chaseTries = 0; return; }
        // An armed timer is never pushed out (an event only adds ids to the set), and a try is counted only when an ask is made.
        if (document.hidden || chaseTimer) return;
        chaseTimer = later(function () {
          chaseTimer = null;
          if (document.hidden) return;
          // Re-checked now: an id the list's own follow-up (or a refresh) settled meanwhile is not asked, and no try is counted.
          chasing.forEach(function (id) { if (!visible.has(id) || !reader.isFast(id)) chasing.delete(id); });
          if (!chasing.size) { chaseTries = 0; return; }
          chaseTries++;
          askNamed(Array.from(chasing));
        }, FOLLOW_MS[Math.min(chaseTries, FOLLOW_MS.length - 1)]);
      }
      function flushNamed() {
        namedTimer = null;
        var list = Array.from(named);
        named.clear();
        askNamed(list);
      }
      function askNamed(list) {
        return reader.pull({ ids: list })
          .then(function () { if (!stopped && o.onChange) o.onChange(handle); }, function (e) { if (!core.isSuperseded(e) && o.onError) o.onError(e); })
          .then(function () { chase(); armSlow(); });
      }
      var handle = {
        get: function (id) { return reader.get(id); },
        reader: reader,
        ids: function () { return Array.from(visible); },
        known: function () { return reader.known(Array.from(visible)); },
        // A live event or the page's own refresh: every visible id again, except refused ones; refresh({refused: true}) asks those too.
        refresh: function (r) { return pull({ all: true, refused: !!(r && r.refused) }); },
        // A data-change event naming these ids: the visible ones are asked again (one ask, exactly those ids, batched over the debounce;
        // no other id is asked and nobody's backoff restarts), the others are marked and asked when they scroll in. An event naming no
        // ids names nothing: the page calls refresh() deliberately.
        changed: function (ids) {
          if (!Array.isArray(ids) || !ids.length) return;
          reader.markDirty(ids);
          ids.forEach(function (id) { if (visible.has(id)) { named.add(id); chasing.add(id); } });
          // Hidden: the names wait, and showing asks them in one batch; nothing is asked on the way back except what an event named.
          if (!named.size || document.hidden) return;
          unlater(namedTimer);
          namedTimer = later(flushNamed, debounce);
        },
        stop: function () { stopped = true; unlater(timer); unlater(namedTimer); unlater(chaseTimer); unlater(slowTimer); boxes.splice(0).forEach(function (b) { if (b.cancel) b.cancel(); }); unlater(follow); totalsHandles.delete(handle); document.removeEventListener("visibilitychange", onShown); if (io) io.disconnect(); if (mo) mo.disconnect(); },
      };
      totalsHandles.add(handle);
      return handle;
    };

    // ── K7 cachedQuery ─────────────────────────────────────────────────────
    // The last answer paints at once, from storage keyed by host, dashboard and args; one read then revalidates it, whatever its age.
    // maxAgeMs decides the label (cached while younger, stale after) and when a later refresh is due, not whether the read happens. Storage is
    // the host adapter's own (`storage()`, the in-app kit only; the published toolkit has none and keeps answers in memory for the page). Any
    // storage failure falls back to memory, and a write that does not fit is skipped.
    var memory = new Map(), cached = new Map();
    function hostStore() { var a = pick(); return a && typeof a.storage === "function" ? a.storage() : null; }
    function storeGet(k) {
      try {
        var store = hostStore(), raw = store ? store.getItem(k) : null, saved = raw != null ? JSON.parse(raw) : null;
        if (saved && typeof saved.at === "number") return saved;
      } catch (e) { /* memory below */ }
      return memory.get(k) || null;
    }
    function storePut(k, entry) {
      memory.set(k, entry);
      try { var store = hostStore(); if (store) store.setItem(k, JSON.stringify(entry)); } catch (e) { /* memory only */ }
    }
    core.cachedQuery = function (name, args, o) {
      o = o || {};
      if (!(o.maxAgeMs > 0)) throw new Error("cachedQuery needs maxAgeMs");
      args = args || {};
      var skey = "cs-kit:1:" + core.host() + ":" + (o.key || window.location.pathname) + ":" + name + ":" + stable(args);
      // One handle per name, args, key and maxAgeMs: asking again with another maxAgeMs gets its own (same stored answer, own label).
      var hkey = skey + "|" + o.maxAgeMs;
      if (cached.has(hkey)) return cached.get(hkey);
      var entry = storeGet(skey), source = entry ? "storage" : null, error = null, seq = 0, listeners = [], settled;
      function peek() {
        if (!entry) return { value: undefined, ageMs: null, state: "none", error: error };
        var ageMs = now() - entry.at;
        if (!isFinite(ageMs)) return { value: entry.value, ageMs: null, state: "stale", error: error };
        ageMs = Math.max(0, ageMs);
        return { value: entry.value, ageMs: ageMs, state: ageMs > o.maxAgeMs ? "stale" : source === "net" ? "fresh" : "cached", error: error };
      }
      function emit() { listeners.slice().forEach(function (cb) { cb(peek()); }); }
      function refresh() {
        var mine = ++seq;
        settled = core.read(name, args, { slot: skey }).then(function (value) {
          if (mine !== seq) return peek();
          entry = { at: now(), value: value };
          source = "net";
          error = null;
          storePut(skey, entry);
          emit();
          return peek();
        }, function (e) {
          if (mine !== seq || core.isSuperseded(e)) return peek();
          error = e;
          emit();
          return peek();
        });
        return settled;
      }
      var handle = {
        peek: peek,
        refresh: refresh,
        onChange: function (cb) { listeners.push(cb); return function () { listeners = listeners.filter(function (l) { return l !== cb; }); }; },
        get ready() { return settled; },
      };
      cached.set(hkey, handle);
      refresh();
      return handle;
    };

    // ── keyed lists, K8 section (resource-store.js) ────────────────────────
    // Render a list into `container` by key, reusing each item's element while its version is unchanged.
    core.keyed = function (container, items, opts) {
      var prev = container.__csKeyed || new Map(), next = new Map();
      var els = items.map(function (item) {
        var k = String(opts.key(item)), v = opts.version ? String(opts.version(item)) : jsonOr(item), hit = prev.get(k), reused = hit && hit.v === v;
        var node = reused ? hit.el : opts.render(item);
        if (reused && opts.update) opts.update(node, item);
        node.dataset.key = k;
        next.set(k, { v: v, el: node });
        return node;
      });
      var keep = new Set(els);
      Array.prototype.slice.call(container.childNodes).forEach(function (n) { if (!keep.has(n)) n.remove(); });
      els.forEach(function (node, i) { if (container.childNodes[i] !== node) container.insertBefore(node, container.childNodes[i] || null); });
      container.__csKeyed = next;
    };
    // One owner per async resource. No scheduling and no shared completion serial. A slot (name + scope) owns its read: fn(slot)
    // starts it and hangs the read's bookkeeping (startedAt, boxes, cancelReads, retry) on the slot, which every view shares.
    function createResourceStore(o) {
      var onChange = o.onChange, slots = new Map(), closedScopes = new Set();
      function get(name, scope) { var slot = slots.get(name); return scope === undefined || (slot && slot.scope === scope) ? slot : undefined; }
      function run(name, scope, fn) {
        if (closedScopes.has(scope)) return Promise.resolve({ applied: false });
        var slot = slots.get(name);
        // Another scope taking the key disposes the slot it replaces: its read is cancelled and its retry timer is gone.
        if (!slot || slot.scope !== scope || slot.disposed) {
          if (slot && slot.dispose && !slot.disposed) slot.dispose();
          slot = { value: null, error: null, token: 0, scope: scope, pending: null };
          slots.set(name, slot);
        }
        slot.cancelled = false;
        var token = ++slot.token;
        function current() { return slots.get(name) === slot && slot.token === token && slot.scope === scope; }
        function finish(value, error) {
          if (!current()) return { applied: false };
          if (!error) slot.value = value;
          slot.error = error;
          slot.pending = null;
          onChange(name, slot);
          return { applied: true, value: slot.value, error: error };
        }
        var result;
        try { result = fn(slot); } catch (e) { return Promise.resolve(finish(null, e)); }
        if (result && typeof result.then === "function") {
          var pending = Promise.resolve(result).then(function (v) { return finish(v, null); }, function (e) { return finish(null, e); });
          slot.pending = pending;
          return pending;
        }
        return Promise.resolve(finish(result, null));
      }
      // Drops the read in flight: a late answer lands on an older token and is ignored. The slot keeps its last value.
      function cancel(name, scope) {
        var slot = get(name, scope);
        if (!slot) return;
        slot.token++;
        slot.pending = null;
        slot.error = null;
        slot.cancelled = true;
        onChange(name, slot);
      }
      function invalidate(scope) {
        closedScopes.add(scope);
        slots.forEach(function (slot) {
          if (slot.scope !== scope) return;
          if (slot.dispose) slot.dispose();
          slot.token++; slot.scope = null; slot.value = null; slot.error = null; slot.pending = null;
        });
      }
      // The slot under this name is this one and its scope is open: the only slot allowed to ask again.
      function alive(name, slot) { return slots.get(name) === slot && !slot.disposed && slot.scope !== null && !closedScopes.has(slot.scope); }
      return { get: get, run: run, cancel: cancel, invalidate: invalidate, alive: alive, each: function (fn) { slots.forEach(fn); } };
    }
    var painters = new Map(), sections = null, revs = 0;
    var BUSY_BACKOFF = [2000, 4000, 8000, 16000];
    // Once per slot change, whoever is looking: stamp a landed read, and ask again after a busy answer (2, 4, 8, then every 16 s)
    // while some view still shows the slot.
    // A view is active when it can be seen: a visible section always, a <details> section while it is open.
    function viewsActive(name) {
      var views = painters.get(name);
      return !!views && Array.from(views).some(function (v) { return v.active && v.active(); });
    }
    function armRetry(slot) {
      var tries = slot.busyTries || 0;
      slot.retryTimer = later(function () {
        slot.retryTimer = null;
        // Hidden, or every view closed: wait. Showing or opening starts the timer again; it never reads on return.
        if (document.hidden || !viewsActive(slot.name)) { slot.retryWaiting = true; return; }
        slot.busyTries = tries + 1;
        if (slot.retry) slot.retry();
      }, BUSY_BACKOFF[Math.min(tries, BUSY_BACKOFF.length - 1)]);
    }
    function settled(slot, views) {
      unlater(slot.retryTimer);
      slot.retryTimer = null;
      slot.retryWaiting = false;
      if (!slot.error && !slot.pending && !slot.cancelled) { slot.readAt = now(); slot.rev = ++revs; slot.busyTries = 0; }
      if (slot.error && core.isBusy(slot.error) && slot.retry && views) armRetry(slot);
    }
    var deferredKeys = [], drainTimer = null;
    // A deferred key's identity: the key and, when named, its scope (so {key, scope} objects compare by value).
    function keyId(k) { return typeof k === "string" ? "s:" + k : "o:" + k.key + ":" + stable(k.scope === undefined ? null : k.scope); }
    // Keys named by an event while the page was hidden are read one at a time, a second apart, once it is shown: no burst on return.
    function drainDeferred() {
      if (drainTimer || !deferredKeys.length) return;
      drainTimer = later(function () {
        drainTimer = null;
        if (document.hidden) return;
        readKey(deferredKeys.shift());
        drainDeferred();
      }, 1000);
    }
    function readKey(k) {
      var key = typeof k === "string" ? k : k.key, slot = sections && sections.get(key, typeof k === "string" ? undefined : k.scope);
      if (slot && slot.retry && slot.readAt != null) slot.retry();
    }
    function sectionStore() {
      if (sections) return sections;
      sections = createResourceStore({
        onChange: function (name, slot) {
          var views = painters.get(name);
          slot.name = name;
          settled(slot, views && views.size && sections.alive(name, slot));
          if (views) views.forEach(function (paint) { paint(slot); });
        },
      });
      document.addEventListener("visibilitychange", function () {
        if (document.hidden) return;
        sections.each(function (slot, name) {
          var views = painters.get(name);
          if (slot.retryWaiting && views && views.size && viewsActive(name) && sections.alive(name, slot)) { slot.retryWaiting = false; armRetry(slot); }
        });
        drainDeferred();
      });
      return sections;
    }
    function minutes(ms) {
      var m = Math.floor(ms / 60000);
      return m < 1 ? "just now" : m < 60 ? m + " min ago" : Math.floor(m / 60) + " h ago";
    }
    // Start the one read of a slot. Same scope and no force: join the read in flight (no cancel, no engine call, startedAt kept).
    // A force, or another scope taking the key, frees the budget slot of the read it replaces first.
    function startRead(key, scope, load, force, alive) {
      var store = sectionStore(), cur = store.get(key);
      if (cur && cur.scope === scope && !force && cur.pending && !cur.disposed) return cur.pending;
      if (cur && cur.cancelReads && force && cur.scope === scope) cur.cancelReads();
      return store.run(key, scope, function (slot) {
        var mine = slot.token, boxes = slot.boxes = [];
        slot.startedAt = now();
        slot.cancelReads = function () { boxes.splice(0).forEach(function (b) { if (b.cancel) b.cancel(); }); };
        slot.retry = function () { if (store.alive(key, slot)) startRead(key, scope, load, true, alive); };
        // The one way a slot ends: its read is cancelled and dropped (a late answer lands on an older token), its retry timer is gone,
        // it can never ask again and nothing can join it: the next read of this key and scope gets a fresh slot.
        slot.dispose = function () {
          slot.disposed = true;
          slot.token++;
          slot.pending = null;
          slot.cancelReads();
          unlater(slot.retryTimer);
          slot.retryTimer = null;
          slot.retry = null;
        };
        function stale() { return slot.token !== mine || store.get(key, scope) !== slot || (alive && !alive()); }
        return load({ scope: scope, read: function (name, args, x) {
          return core.tracked(boxes, function (box) { return core.read(name, args, Object.assign({ box: box, stale: stale }, x)); });
        } });
      });
    }
    // section(el, {key, load, render, when: 'open' | 'visible', scope, isEmpty, emptyText, stillMs}): one bounded read when a
    // <details> opens or the region scrolls into view. load({read, scope}) resolves the value, or {unknown: reason}. A section is
    // a view over the store slot of its key and scope: the slot owns the read, so two sections of one key and scope share one
    // engine call, one elapsed time and one Cancel. The view draws a skeleton, empty, unknown (with the reason), error with Retry,
    // "Waiting for the Mac" (the Mac said busy: asked again after 2, 4, 8, 16 s), "Read N min ago" and "Still reading (N s)" with
    // Cancel (which frees the read's budget slot at once). `scope` (a value or a function) names the owner of the answer: another
    // scope taking the key cancels the read it replaces, and an answer that lands after the scope changed, or the scope was closed
    // with closeScope(), is dropped. Use a fresh token per visit: a closed scope stays closed. A clock tick rewrites only the status
    // text, never the content, so focus and selection survive it.
    core.section = function (host, o) {
      var store = sectionStore(), ticker = null, off = null, unwatch = null, stopped = false, contentKey = "", actionKey = "", lastSlot = null;
      var stillMs = o.stillMs == null ? 15000 : o.stillMs, when = o.when || (host.tagName === "DETAILS" ? "open" : "visible");
      var body = Array.from(host.children).find(function (c) { return c.classList.contains("cs-section-body"); });
      if (!body) { body = el("div", "cs-section-body"); host.appendChild(body); }
      var content = el("div", "cs-section-content"), line = el("div", "cs-section-meta"), lineText = document.createTextNode("");
      line.hidden = true;
      line.append(lineText);
      body.replaceChildren(content, line);
      style();
      var retry = { label: "Retry", onClick: function () { run(true); } }, stopReading = { label: "Cancel", onClick: cancel };
      // Sections do not refresh themselves, so a read a minute old offers to read again.
      var refreshNow = { label: "Refresh", onClick: retry.onClick };
      function scopeNow() { var s = typeof o.scope === "function" ? o.scope() : o.scope; return s === undefined ? "page" : s; }
      function slotNow() { return store.get(o.key, scopeNow()); }
      function isEmpty(v) { return o.isEmpty ? o.isEmpty(v) : Array.isArray(v) ? !v.length : v == null; }
      // The content is rebuilt only when what it shows changes: a new answer, the skeleton, nothing.
      function paintContent(slot) {
        var key = slot && slot.rev != null ? "value:" + slot.rev : slot && slot.pending ? "skeleton" : "none";
        if (key === contentKey) return;
        contentKey = key;
        if (key === "skeleton") {
          var sk = el("div", "cs-section-skeleton");
          sk.setAttribute("aria-busy", "true");
          sk.append(el("span"), el("span"), el("span"));
          content.replaceChildren(sk);
        } else if (key === "none") content.replaceChildren();
        else if (slot.value && slot.value.unknown) content.replaceChildren(el("div", "cs-section-error", unknownLabel(slot.value.unknown)));
        else if (isEmpty(slot.value)) content.replaceChildren(el("div", "cs-empty", o.emptyText || "Nothing here yet."));
        else {
          var wrap = el("div", "cs-section-value");
          try {
            o.render(slot.value, wrap);
            content.replaceChildren(wrap);
          } catch (e) {
            console.error(e);
            content.replaceChildren(el("div", "cs-section-error", "Could not show this."));
          }
        }
      }
      // The status line: its text node is rewritten in place; its button only when the action changes. Elapsed time is the slot's.
      function paintMeta(slot) {
        var loading = !!(slot && slot.pending), err = slot && slot.error, text = "", action = null, elapsed = loading ? now() - slot.startedAt : 0;
        if (slot && slot.cancelled) { text = "Cancelled."; action = retry; }
        else if (err && core.isBusy(err)) text = WAITING;
        else if (err) { var why = core.errorText(err); text = why === GENERIC_ERROR ? GENERIC_ERROR + "." : GENERIC_ERROR + ". " + why; action = retry; }
        else if (loading && elapsed >= stillMs) { text = "Still reading (" + (elapsed < 120000 ? Math.floor(elapsed / 1000) + " s" : Math.floor(elapsed / 60000) + " min") + ")"; action = stopReading; }
        else if (loading && core.busy()) text = WAITING;
        else if (slot && slot.readAt != null) {
          var age = now() - slot.readAt;
          text = "Read " + minutes(age) + (loading ? " · updating" : "");
          if (!loading && age >= 60000) action = refreshNow;
        }
        lineText.data = text;
        line.hidden = !text;
        var key = action ? action.label : "";
        if (key === actionKey) return;
        actionKey = key;
        while (line.childNodes.length > 1) line.lastChild.remove();
        if (action) {
          var b = el("button", "cs-btn is-sm", action.label);
          b.type = "button";
          b.addEventListener("click", action.onClick);
          line.append(b);
        }
      }
      // One timer: at stillMs while reading, then every 10 s to keep "Still reading (N s)" true (30 s once the read has landed).
      function tick() {
        unlater(ticker);
        ticker = null;
        var slot = slotNow(), loading = !!(slot && slot.pending);
        if (stopped || !(loading || (slot && slot.readAt != null))) return;
        var wait = loading ? (now() - slot.startedAt < stillMs ? stillMs - (now() - slot.startedAt) : 10000) : 30000;
        ticker = later(function () { ticker = null; paintMeta(slotNow()); tick(); }, wait);
      }
      function paint(slot) { if (slot) lastSlot = slot; paintContent(slot); paintMeta(slot); tick(); }
      function repaint(slot) { if (!stopped && slot.scope === scopeNow()) paint(slot); }
      repaint.active = function () { return !stopped && (when !== "open" || host.open); };
      // Opening (or scrolling in) starts a busy retry that waited for this view again; it never reads on the way.
      function resumeRetry() {
        var slot = slotNow();
        if (slot && slot.retryWaiting && !document.hidden && sections.alive(o.key, slot)) { slot.retryWaiting = false; armRetry(slot); }
      }
      if (!painters.has(o.key)) painters.set(o.key, new Set());
      painters.get(o.key).add(repaint);
      unwatch = core.budget().onChange(function () { var slot = slotNow(); if (slot && slot.pending) paintMeta(slot); });
      function run(force) {
        resumeRetry();
        var scope = scopeNow(), slot = store.get(o.key, scope);
        if (!force && slot && slot.pending) { paint(slot); return slot.pending; }
        // A section that has read, was cancelled or failed shows that again, however long ago, however long it was hidden and however
        // often it scrolls back in: only Retry or refresh() (force), a data-change event that names its key (dataChanged) or the busy
        // backoff reads again.
        if (!force && slot && !slot.disposed && (slot.readAt != null || slot.error || slot.cancelled)) return paint(slot);
        var started = startRead(o.key, scope, o.load, force, function () { return scopeNow() === scope; });
        paint(store.get(o.key, scope));
        started.then(function (r) { if (r && !r.applied) paint(store.get(o.key, scopeNow())); });
        return started;
      }
      // Cancel frees the reads' budget slots at once; the slot drops the read in flight, so a late answer is ignored.
      function cancel() {
        var slot = slotNow();
        if (!slot || !slot.pending) return;
        if (slot.cancelReads) slot.cancelReads();
        store.cancel(o.key, scopeNow());
      }
      function arm() {
        if (when === "open") {
          var onToggle = function () { if (host.open) run(); };
          host.addEventListener("toggle", onToggle);
          off = function () { host.removeEventListener("toggle", onToggle); };
          if (host.open) run();
        } else if (typeof IntersectionObserver !== "undefined") {
          var io = new IntersectionObserver(function (entries) { if (entries.some(function (e) { return e.isIntersecting; })) run(); });
          io.observe(host);
          off = function () { io.disconnect(); };
        } else run();
      }
      arm();
      // A key an event named while the page was hidden and no section existed is consumed now: this section's first read is fresh.
      deferredKeys = deferredKeys.filter(function (k) {
        if ((typeof k === "string" ? k : k.key) !== o.key) return true;
        return typeof k !== "string" && k.scope !== undefined && k.scope !== scopeNow();
      });
      return {
        refresh: function () { return run(true); },
        cancel: cancel,
        get loaded() { var slot = slotNow(); return !!slot && slot.readAt != null; },
        // The last view of a key leaving disposes its slot: no read is left running and no timer fires later.
        stop: function () {
          stopped = true;
          unlater(ticker);
          if (off) off();
          if (unwatch) unwatch();
          var views = painters.get(o.key);
          views.delete(repaint);
          // The slot this view last painted, not whatever its scope names now (the page may have moved on before stopping).
          if (!views.size && lastSlot && lastSlot.dispose && !lastSlot.disposed) lastSlot.dispose();
        },
      };
    };
    core.closeScope = function (scope) { sectionStore().invalidate(scope); };
    // A live data-change event: dataChanged({ids, keys}). `ids` (row or story ids) go to every visibleTotals: the visible ones are asked
    // again, one ask with just those ids, and the rest are marked and asked when they scroll in. `keys` (a section key, or {key, scope})
    // read again quietly in place, only the sections named. An event naming neither does nothing: nothing requeries a whole list or story.
    core.dataChanged = function (ev) {
      ev = ev || {};
      if (Array.isArray(ev.ids)) totalsHandles.forEach(function (h) { h.changed(ev.ids); });
      (ev.keys || []).forEach(function (k) {
        if (document.hidden) { if (!deferredKeys.some(function (d) { return keyId(d) === keyId(k); }) && deferredKeys.length < 200) deferredKeys.push(k); } else readKey(k);
      });
    };

    // ── K10 keyedRegion (focus.js, patches.js) ─────────────────────────────
    // Capture meaning before replacing DOM; every restoration uses the same fallback chain.
    function createFocusIntents(p) {
      var root = p.root, doc = p.doc || root.ownerDocument;
      function attr(name, value) { return "[" + name + '="' + CSS.escape(String(value)) + '"]'; }
      function available(node) { return node && !node.disabled && !node.closest("[hidden], [inert]"); }
      function controls(row) { return Array.from(row.querySelectorAll("button,a[href],input,select,textarea,summary,[tabindex]")).filter(function (n) { return available(n) && n.tabIndex >= 0; }); }
      function find(scope, selector) { return selector ? (scope.matches && scope.matches(selector) ? scope : scope.querySelector(selector)) : null; }
      function point(node, offset) {
        var parent = node.nodeType === 1 ? node : node.parentElement, scope = (parent && parent.closest("[data-row-focus],[id]")) || root;
        var key = scope.id ? "#" + CSS.escape(scope.id) : scope.dataset && scope.dataset.rowFocus ? attr("data-row-focus", scope.dataset.rowFocus) : null, path = [];
        for (var at = node; at !== scope; at = at.parentNode) { if (!at.parentNode) return null; path.unshift(Array.from(at.parentNode.childNodes).indexOf(at)); }
        return { key: key, path: path, offset: offset };
      }
      function textSelection() {
        var sel = doc.getSelection();
        if (!sel || sel.isCollapsed || !root.contains(sel.anchorNode) || !root.contains(sel.focusNode)) return null;
        var range = sel.getRangeAt(0);
        return { start: point(range.startContainer, range.startOffset), end: point(range.endContainer, range.endOffset), text: sel.toString(), forward: sel.anchorNode === range.startContainer && sel.anchorOffset === range.startOffset };
      }
      function restoreSelection(saved) {
        if (!saved || !saved.start || !saved.end) return;
        function resolve(pt) {
          var node = pt.key ? root.querySelector(pt.key) : root;
          if (pt.key && root.matches && root.matches(pt.key)) node = root;
          for (var i = 0; i < pt.path.length; i++) node = node && node.childNodes[pt.path[i]];
          return node && pt.offset <= (node.nodeType === 3 ? node.length : node.childNodes.length) ? node : null;
        }
        var start = resolve(saved.start), end = resolve(saved.end);
        if (!start || !end) return;
        var range = doc.createRange();
        range.setStart(start, saved.start.offset);
        range.setEnd(end, saved.end.offset);
        if (range.toString() !== saved.text) return;
        var sel = doc.getSelection();
        if (saved.forward) sel.setBaseAndExtent(start, saved.start.offset, end, saved.end.offset); else sel.setBaseAndExtent(end, saved.end.offset, start, saved.start.offset);
      }
      function capture(element) {
        element = element || doc.activeElement;
        var selection = textSelection();
        if (!element || element === doc.body || !root.contains(element)) return selection ? { focused: false, selection: selection } : null;
        var row = element.closest("[data-row-focus]"), rowId = row ? row.dataset.rowFocus : null;
        var key = ["data-focus-key", "data-panel-key", "data-sort", "data-open", "data-retry", "data-group", "name"].find(function (n) { return element.hasAttribute(n); });
        var controlId = key ? attr(key, element.getAttribute(key)) : element.id ? "#" + CSS.escape(element.id) : null;
        var sectionNode = element.closest("section");
        var heading = sectionNode && sectionNode.querySelector("h2[id]"), sectionId = heading ? heading.id : null;
        return { selection: selection, rowId: rowId, controlId: controlId, sectionId: sectionId, position: row ? Math.max(0, controls(row).indexOf(element)) : 0, element: element };
      }
      function restore(intent, r) {
        if (!intent) return;
        r = r || {};
        restoreSelection(intent.selection);
        if (intent.focused === false) return;
        var rowNode = intent.rowId ? root.querySelector(attr("data-row-focus", intent.rowId)) : null, row = available(rowNode) ? rowNode : null;
        var target = available(intent.element) && root.contains(intent.element) ? intent.element : null;
        if (target == null) target = find(row || root, intent.controlId);
        if (!available(target)) target = null;
        if (!target && intent.controlId) target = find(root, intent.controlId);
        if (!available(target)) target = null;
        var fallback = false;
        if (!target && row) { fallback = true; var candidates = controls(row); target = candidates[Math.min(intent.position || 0, candidates.length - 1)] || row; }
        if (!target) fallback = true;
        if (!target) target = (intent.sectionId ? doc.getElementById(intent.sectionId) : null) || root.querySelector("h1,h2");
        if (target) {
          if (target.tabIndex < 0) target.setAttribute("tabindex", "-1");
          target.focus({ preventScroll: true });
          if (r.reveal || fallback) target.scrollIntoView && target.scrollIntoView({ block: "nearest" });
        }
      }
      var view = doc.defaultView, lastIntent = null, guardianFrame = null;
      function remember(event) { var intent = capture(event.target); if (intent && intent.focused !== false) lastIntent = intent; }
      function guard(event) {
        if (event && event.target) remember(event);
        // Native date editors can omit focusin; focusout still names the destination.
        if (event && event.relatedTarget) { if (root.contains(event.relatedTarget)) remember({ target: event.relatedTarget }); return; }
        if (guardianFrame != null) return;
        guardianFrame = view.requestAnimationFrame(function () {
          guardianFrame = null;
          // A deliberate Tab/Escape into the host must remain outside this document.
          if (lastIntent && doc.hasFocus() && (doc.activeElement === doc.body || !root.contains(doc.activeElement))) {
            if (root.contains(lastIntent.element) && available(lastIntent.element)) { lastIntent = null; return; }
            restore(Object.assign({}, lastIntent, { selection: null }));
          }
        });
      }
      root.addEventListener("focusin", remember, true);
      root.addEventListener("focusout", guard, true);
      // WebKit does not emit focusout when the focused node is removed.
      var observer = new view.MutationObserver(function () {
        if (lastIntent && (!root.contains(lastIntent.element) || !available(lastIntent.element)) && doc.hasFocus() && (doc.activeElement === doc.body || !root.contains(doc.activeElement))) guard();
      });
      observer.observe(root, { childList: true, subtree: true, attributes: true, attributeFilter: ["disabled", "hidden", "inert"] });
      function stop() {
        observer.disconnect();
        root.removeEventListener("focusin", remember, true);
        root.removeEventListener("focusout", guard, true);
        if (guardianFrame != null) view.cancelAnimationFrame(guardianFrame);
        lastIntent = null;
      }
      return { capture: capture, restore: restore, stop: stop };
    }
    // keyed owns reconciliation; this layer only postpones protected structure (a focused or selected row) and gestures.
    function createPatches(p) {
      var root = p.root, doc = p.doc || root.ownerDocument, records = new WeakMap(), pending = new Map(), texts = new Map(), refreshes = new Map();
      var gesturing = false, stopped = false, releaseTimer = null;
      function key(opts, item) { return String(opts.key(item)); }
      function version(opts, item) { return String(opts.version ? opts.version(item) : jsonOr(item)); }
      function selected(node) {
        var sel = doc.getSelection();
        if (!sel || !sel.rangeCount || sel.isCollapsed || !node.isConnected) return false;
        return sel.getRangeAt(0).intersectsNode(node);
      }
      function associated(node) {
        var rowNode = node && node.closest && node.closest("[data-row-id]"), id = rowNode && rowNode.dataset.rowId;
        return id ? root.querySelector('[data-row-focus="' + CSS.escape(id) + '"]') : null;
      }
      function touching(node) {
        var active = doc.hasFocus() ? doc.activeElement : null;
        return !!(active && (node.contains(active) || node.contains(associated(active))) || selected(node));
      }
      function text(node, value) {
        value = String(value == null ? "" : value);
        if (node.textContent === value) { texts.delete(node); return; }
        if (selected(node)) { texts.set(node, value); return; }
        if (node.childNodes.length === 1 && node.firstChild.nodeType === 3) node.firstChild.data = value; else node.textContent = value;
        texts.delete(node);
      }
      function keyed(container, items, opts, passive) {
        var previous = records.get(container), incoming = new Map(items.map(function (item) { return [key(opts, item), item]; }));
        var order = items.map(function (item) { return key(opts, item); });
        var structural = previous && (order.join("\0") !== previous.order.join("\0") || items.some(function (item) { return previous.versions.get(key(opts, item)) !== version(opts, item); }));
        // A genuine deletion of the protected row is handled by the focus guardian.
        var protectedRow = Array.from(container.children).some(function (node) { return incoming.has(node.dataset.key) && touching(node); });
        var effective = items, versions = new Map(items.map(function (item) { return [key(opts, item), version(opts, item)]; }));
        if (passive && structural && protectedRow) {
          pending.set(container, { items: items, opts: opts });
          effective = previous.items.map(function (item) { return incoming.get(key(opts, item)) || item; });
          versions = previous.versions;
        } else pending.delete(container);
        core.keyed(container, effective, {
          key: opts.key, version: function (item) { return versions.get(key(opts, item)) || version(opts, item); },
          render: function (item) { var node = opts.render(item); if (opts.update) opts.update(node, item); return node; }, update: opts.update,
        });
        records.set(container, { items: effective, order: effective.map(function (item) { return key(opts, item); }), versions: versions });
      }
      // Gestures protect the whole page: even facts in another row can move a target.
      function defer(owner, refresh) { if (!gesturing) return false; refreshes.set(owner, refresh); return true; }
      function flush() {
        if (stopped || gesturing) return;
        var work = Array.from(refreshes.values());
        refreshes.clear();
        work.forEach(function (refresh) { refresh(); });
        texts.forEach(function (value, node) { if (!node.isConnected) texts.delete(node); else if (!selected(node)) text(node, value); });
        Array.from(pending).forEach(function (pair) { if (pair[0].isConnected) keyed(pair[0], pair[1].items, pair[1].opts, true); else pending.delete(pair[0]); });
      }
      function down(event) {
        if (event.button !== 0 || event.isPrimary === false) return;
        if (releaseTimer != null) { unlater(releaseTimer); releaseTimer = null; }
        gesturing = true;
      }
      // The click handler consumes the held target before queued reads can patch it.
      function end() { if (releaseTimer != null) { unlater(releaseTimer); releaseTimer = null; } gesturing = false; queueMicrotask(flush); }
      function up() { if (gesturing && releaseTimer == null) releaseTimer = later(end, 0); }
      function exit(event) { if (gesturing && event.buttons && !event.relatedTarget) end(); }
      function leave() { queueMicrotask(flush); }
      var win = doc.defaultView, listeners = [["pointerdown", down, true], ["pointerup", up, true], ["pointerleave", exit, true], ["mouseleave", exit, true], ["click", end, false], ["auxclick", end, true], ["dragend", end, true], ["pointercancel", end, true], ["focusout", leave, true], ["selectionchange", leave, false]];
      listeners.forEach(function (l) { doc.addEventListener(l[0], l[1], l[2]); });
      win.addEventListener("blur", end);
      function stop() {
        stopped = true;
        end();
        pending.clear(); texts.clear(); refreshes.clear();
        listeners.forEach(function (l) { doc.removeEventListener(l[0], l[1], l[2]); });
        win.removeEventListener("blur", end);
      }
      return { keyed: keyed, text: text, defer: defer, stop: stop, flush: flush };
    }
    // keyedRegion(el, scope): regions repaint in place on a selection change, keeping focus, selection, scroll and gesture
    // deferral. paint(items, {key, render, version, update}, forScope): a paint for a scope that is no longer current is dropped.
    core.keyedRegion = function (root, scope) {
      var current = scope, intents = createFocusIntents({ root: root }), patches = createPatches({ root: root });
      return {
        setScope: function (next) { current = next; },
        paint: function (items, opts, forScope) {
          if (forScope !== undefined && forScope !== current) return false;
          var intent = intents.capture();
          patches.keyed(root, items, opts, true);
          intents.restore(intent);
          return true;
        },
        text: patches.text,
        defer: patches.defer,
        flush: patches.flush,
        stop: function () { intents.stop(); patches.stop(); },
      };
    };

    // ── K1 boot ────────────────────────────────────────────────────────────
    // boot({render, queries: {alias: {name, args, maxAgeMs}}, key, onError}) -> {host, call, subscribe, state, refresh, stop}
    // render(ctx) runs at once (cached answers are there synchronously) and again whenever a query changes; it must be
    // idempotent. ctx.state.data[alias] is cachedQuery's {value, ageMs, state, error}. The host comes from the bridge object
    // that is present (core.host), so one entry file runs in the app and on the share host.
    core.boot = function (o) {
      o = o || {};
      var adapter = pick(), handles = {}, offs = [], stopped = false;
      var ctx = {
        host: adapter.name, state: { host: adapter.name, ready: !adapter.ready, error: null, data: {} },
        call: function (name, args, opts) { return core.read(name, args, opts); }, subscribe: adapter.subscribe,
      };
      function paint() {
        if (stopped || typeof o.render !== "function") return;
        try { o.render(ctx); } catch (e) { if (o.onError) o.onError(e); else console.error(e); }
      }
      Object.keys(o.queries || {}).forEach(function (alias) {
        var q = o.queries[alias], h = core.cachedQuery(q.name, q.args || {}, { key: o.key, maxAgeMs: q.maxAgeMs || 60000 });
        handles[alias] = h;
        ctx.state.data[alias] = h.peek();
        offs.push(h.onChange(function (v) { ctx.state.data[alias] = v; paint(); }));
      });
      if (adapter.ready) {
        adapter.ready().then(function () { ctx.state.ready = true; paint(); }, function (e) { ctx.state.error = String((e && e.message) || e); paint(); });
      }
      paint();
      return {
        host: adapter.name, state: ctx.state, call: ctx.call, subscribe: adapter.subscribe,
        refresh: function () { return Promise.all(Object.keys(handles).map(function (a) { return handles[a].refresh(); })); },
        stop: function () { stopped = true; offs.forEach(function (off) { off(); }); },
      };
    };

    core.api = {
      coreVersion: core.version, dataChanged: core.dataChanged, errorText: core.errorText, errorKind: core.errorKind, errorCodes: core.errorCodes, boot: core.boot, paced: core.paced, budget: core.budget, value: core.value, valueState: core.valueState, stateBanner: core.stateBanner,
      partial: core.partial, batchTotals: core.batchTotals, visibleTotals: core.visibleTotals, cachedQuery: core.cachedQuery,
      section: core.section, closeScope: core.closeScope, keyedRegion: core.keyedRegion, keyed: core.keyed,
    };
    return core;
  })());
