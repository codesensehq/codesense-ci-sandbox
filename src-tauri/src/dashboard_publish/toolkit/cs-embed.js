// cs-embed 1
// The CodeSense embed pipe (SV-INP-29): one dependency-free source for live
// content that runs inside another page, such as an HTML message in a
// sandboxed frame on a published dashboard. The same SDK calls
// the content makes on the Mac (queryContext, subscribeContext, inbox) go
// through a Transport; this file has two, plus the forwarder that makes the
// nested one safe:
//
//   direct   the page calls its own upstream (cs-live.js CS.query) itself
//   nested   the embed posts {cs:"embed"} to its parent; the page's forwarder
//            checks the grant and asks the page's own upstream
//
// The outermost publisher grants capabilities, never the content: the grant is
// cs-embeds.json, which publish generates from the approved dashboard.yml
// live.embeds block. A missing, malformed or empty grant denies everything.
//
// live.embeds.run says for whom a message's own scripts run at all: none (the
// default; nobody), owner (only the signed-in owner) or everyone (every invited
// viewer). With owner or everyone, agent-authored scripts run on those devices
// with the same trust as on the Mac: they read what the grant shares and can
// reveal the device's IP (WebRTC). The role comes from the shell's hello, which a
// page cannot forge. Nobody acts through this pipe: it has no action verbs, so a
// viewer (or anyone) can only read what the grant names.
//
//   CSEmbed.parseGrant(json)                     -> grant | null (null denies all)
//   CSEmbed.mayRun(grant, role)                  -> true when scripts may run for role ("owner" | "viewer")
//   CSEmbed.createPool(upstream, opts)           -> pool shared by every embed on the page
//        upstream: {query(name, args) -> Promise<answer>}; for cs-live.js,
//        {query: function (n, a) { return CS.query(n, a).then(function (m) { return m.answer; }); }}
//   CSEmbed.createForwarder({pool, grant, role, frame, embedId}) -> {handle(event), setVisible(bool), destroy()}
//        page side: window.addEventListener("message", fwd.handle); the only path from an embed.
//        role is a function returning "owner" | "viewer" | null (CS.role); it is read on every call
//   CSEmbed.directTransport(pool, grant, role)   -> Transport (role as above, optional)
//   CSEmbed.nestedTransport(parentWindow)        -> Transport (embed side)
//   CSEmbed.createSdk(transport)                 -> the SDK object an embed sees as window["codesense"]
//   CSEmbed.buildSrcdoc(body, opts)              -> a srcdoc string: CSP, this SDK, then body
//        opts.head: extra markup for the head, under the closed CSP (inline <style> only). The host page must put the
//        --cs-* theme tokens here (--cs-bg, --cs-fg, --cs-muted-fg, --cs-border, --cs-surface, --cs-accent, --cs-link,
//        --cs-success, --cs-warning, --cs-danger; light and dark), as the in-app Inbox does, or messages written for
//        the Mac can render unreadably (light text on a light bubble)
//   CSEmbed.source                               -> this module's own text, for srcdoc
//
// Every denial is an Error with .code: not_declared (outside the grant),
// unavailable (this pipe cannot do it, e.g. openFile on a hosted page),
// rate_limited, invalid_args, mac_offline, no_answer, failed.
var CSEmbed = (function csEmbedFactory() {
  "use strict";
  var VERSION = 1;
  var MAX_ENVELOPE_BYTES = 16384;
  var POLL_MIN_S = 15;
  var EMBED_CALLS_PER_MIN = 10;
  var EMBED_IN_FLIGHT = 2;
  var EMBED_SUBS = 4;
  // About half of the viewer's 60 asks a minute and 4 in flight, shared by every embed on the page.
  var PAGE_CALLS_PER_MIN = 30;
  var PAGE_IN_FLIGHT = 2;
  var WINDOW_MS = 60000;

  // Verbs this pipe can serve, with the arguments each accepts. Anything else in an
  // envelope is dropped, so a newer local shape never reaches the Mac unreviewed.
  var QUERY_OF = { queryContext: "queryContext", inbox: "inbox" };
  var ARG_KEYS = {
    queryContext: ["match", "sort", "limit", "groupBy", "where", "filters", "select"],
    inbox: ["scope", "limit", "agentId", "sessionId", "role"],
  };
  var INBOX_FIELD_GROUPS = {
    text: ["text"],
    meta: ["to", "toName", "replyTo", "replyToName", "contentType"],
    media: ["attachments"],
  };
  var KNOWN_UNAVAILABLE = { openFile: true };

  function fail(code, message) {
    var e = new Error(message || code);
    e.code = code;
    return e;
  }

  // The relay and toolkit speak their own words; the embed hears the local shape.
  var ERROR_CODES = {
    mac_offline: "mac_offline",
    rate_limited: "rate_limited",
    no_answer: "no_answer",
    query_not_declared: "not_declared",
    not_declared: "not_declared",
    not_approved: "not_declared",
    arg_not_allowed: "arg_not_allowed",
    invalid_args: "invalid_args",
    too_large: "too_large",
    answer_too_large: "too_large",
    unavailable: "unavailable",
  };
  function codeOf(error) {
    var raw = error && (error.code || error.message);
    return Object.prototype.hasOwnProperty.call(ERROR_CODES, raw) ? ERROR_CODES[raw] : "failed";
  }

  function isObject(v) {
    return v !== null && typeof v === "object" && !Array.isArray(v);
  }

  // ---- the grant ----

  function stringList(v) {
    if (!Array.isArray(v) || v.length > 64) return null;
    for (var i = 0; i < v.length; i++) if (typeof v[i] !== "string" || v[i].length > 64) return null;
    return v.slice();
  }

  function parseGrant(value) {
    var g = value;
    if (typeof g === "string") {
      try { g = JSON.parse(g); } catch (e) { return null; }
    }
    if (!isObject(g) || g.v !== 1) return null;
    var queries = stringList(g.queries);
    var streams = stringList(g.streams === undefined ? [] : g.streams);
    if (!queries || !streams || !isObject(g.fields === undefined ? {} : g.fields)) return null;
    var fields = {};
    var given = g.fields || {};
    for (var q in given) {
      var list = stringList(given[q]);
      if (!list) return null;
      fields[q] = list;
    }
    if (!queries.length && !streams.length) return null;
    var run = g.run === undefined ? "none" : g.run;
    if (run !== "none" && run !== "owner" && run !== "everyone") return null;
    // Same shape as the file, so a parsed grant parses again to itself.
    return { v: 1, queries: queries, streams: streams, fields: fields, run: run, sha: typeof g.sha === "string" ? g.sha : null };
  }

  // Scripts run only where the publisher chose: never for none, for the owner under owner,
  // for any signed-in viewer under everyone.
  function mayRun(grantValue, role) {
    var g = parseGrant(grantValue);
    if (!g || g.run === "none") return false;
    if (role === "owner") return true;
    return g.run === "everyone" && role === "viewer";
  }

  function fieldsOf(grant, verb) {
    return (grant && grant.fields && grant.fields[QUERY_OF[verb]]) || [];
  }

  // ---- argument checks (the Mac translates and checks again; this drops what is not asked for) ----

  function cleanValue(v, depth) {
    if (depth > 6) throw fail("invalid_args", "arguments nest too deeply");
    if (v === null || typeof v === "number" || typeof v === "boolean") return v;
    if (typeof v === "string") {
      if (v.length > 500) throw fail("invalid_args", "a value is too long");
      return v;
    }
    if (Array.isArray(v)) {
      if (v.length > 64) throw fail("invalid_args", "a list is too long");
      return v.map(function (x) { return cleanValue(x, depth + 1); });
    }
    if (isObject(v)) {
      var out = {};
      var keys = Object.keys(v);
      if (keys.length > 32) throw fail("invalid_args", "an object has too many keys");
      keys.forEach(function (k) {
        // JSON-like data from an embed may carry an own "__proto__"; never copy it onto a plain object.
        if (k === "__proto__" || k === "constructor" || k === "prototype") throw fail("invalid_args", k + " is not allowed as a key");
        out[k] = cleanValue(v[k], depth + 1);
      });
      return out;
    }
    throw fail("invalid_args", "arguments must be plain data");
  }

  function sortField(key) {
    return typeof key === "string" ? key.replace(/^-/, "") : "";
  }

  function normalizeArgs(verb, args, grant) {
    var given = args === undefined || args === null ? {} : args;
    if (!isObject(given)) throw fail("invalid_args", "arguments must be an object");
    var out = {};
    ARG_KEYS[verb].forEach(function (k) { if (given[k] !== undefined) out[k] = cleanValue(given[k], 0); });
    if (verb !== "queryContext") return out;
    var allowed = fieldsOf(grant, verb);
    function check(field) {
      if (typeof field !== "string" || allowed.indexOf(field) < 0) throw fail("not_declared", field + " is not granted to embeds");
    }
    if (out.match !== undefined) {
      if (!isObject(out.match)) throw fail("invalid_args", "match must be an object");
      Object.keys(out.match).forEach(check);
    }
    if (out.filters !== undefined) {
      if (!Array.isArray(out.filters)) throw fail("invalid_args", "filters must be a list");
      out.filters.forEach(function (f) { check(isObject(f) ? f.key : null); });
    }
    if (out.sort !== undefined) {
      if (!Array.isArray(out.sort)) throw fail("invalid_args", "sort must be a list");
      out.sort.forEach(function (key) { if (sortField(key) !== "mtime") check(sortField(key)); });
    }
    if (out.groupBy !== undefined) check(out.groupBy);
    if (out.where !== undefined) {
      if (!isObject(out.where)) throw fail("invalid_args", "where must be an object");
      Object.keys(out.where).forEach(function (k) {
        if (k !== "kinds") throw fail("invalid_args", "where." + k + " is not available here");
      });
    }
    if (out.select !== undefined) {
      if (!Array.isArray(out.select)) throw fail("invalid_args", "select must be a list");
      out.select.forEach(check);
    } else {
      out.select = allowed.slice();
    }
    return out;
  }

  // What the embed gets back is cut to what it was granted, even when the page itself shares more.
  function filterAnswer(verb, answer, grant) {
    if (!isObject(answer)) return answer;
    var allowed = fieldsOf(grant, verb);
    if (verb === "queryContext") {
      var out = {};
      Object.keys(answer).forEach(function (k) { if (k !== "rows") out[k] = answer[k]; });
      out.rows = (Array.isArray(answer.rows) ? answer.rows : []).map(function (row) {
        var r = { id: row.id, relPath: row.relPath, fileName: row.fileName, kind: row.kind, mtime: row.mtime, fields: {} };
        var f = isObject(row.fields) ? row.fields : {};
        allowed.forEach(function (name) { if (name !== "title" && f[name] !== undefined) r.fields[name] = f[name]; });
        if (allowed.indexOf("title") >= 0 && row.title !== undefined) r.title = row.title;
        return r;
      });
      return out;
    }
    if (verb === "inbox") {
      var hidden = [];
      Object.keys(INBOX_FIELD_GROUPS).forEach(function (g) {
        if (allowed.indexOf(g) < 0) hidden = hidden.concat(INBOX_FIELD_GROUPS[g]);
      });
      var strip = function (m) {
        var c = {};
        Object.keys(m).forEach(function (k) { if (hidden.indexOf(k) < 0) c[k] = m[k]; });
        return c;
      };
      var copy = {};
      Object.keys(answer).forEach(function (k) { copy[k] = answer[k]; });
      if (Array.isArray(answer.messages)) copy.messages = answer.messages.map(strip);
      return copy;
    }
    return answer;
  }

  // ---- the pool: limits and shared polling, one per page ----

  function createPool(upstream, opts) {
    opts = opts || {};
    var now = opts.now || Date.now;
    var setT = opts.setTimeout || setTimeout;
    var clearT = opts.clearTimeout || clearTimeout;
    var stamps = [];
    var inFlight = 0;
    var pollers = Object.create(null);

    // At most PAGE_IN_FLIGHT asks at once; a few more wait their turn, the rest are refused.
    var waiters = [];
    function slot() {
      if (inFlight < PAGE_IN_FLIGHT) { inFlight++; return Promise.resolve(); }
      if (waiters.length >= 8) return Promise.reject(fail("rate_limited", "the page's live asks are backed up"));
      return new Promise(function (ok) { waiters.push(ok); });
    }
    function release() {
      inFlight--;
      var next = waiters.shift();
      if (next) { inFlight++; next(); }
    }

    // One upstream ask, counted against the page budget when it starts.
    function ask(verb, args) {
      return slot().then(function () {
        var t = now();
        while (stamps.length && t - stamps[0] >= WINDOW_MS) stamps.shift();
        if (stamps.length >= PAGE_CALLS_PER_MIN) {
          release();
          throw fail("rate_limited", "the page's live budget is spent");
        }
        stamps.push(t);
        return Promise.resolve().then(function () { return upstream.query(QUERY_OF[verb], args); })
          .then(function (a) { release(); return a; }, function (e) { release(); throw fail(codeOf(e)); });
      });
    }

    // Identical subscriptions share one poll; a poll with no visible watcher waits.
    // charge() (optional) is the subscribing embed's own allowance: each poll tick costs every
    // subscribing embed one read, whoever else shares the upstream ask, and an embed that cannot
    // pay hears rate_limited for that tick instead of silence. prepaid: the registration already paid
    // for the first read.
    function watch(verb, args, everyS, listener, charge, prepaid) {
      var key = verb + "\u0000" + JSON.stringify(args);
      var p = pollers[key];
      if (!p) {
        p = pollers[key] = { verb: verb, args: args, every: POLL_MIN_S, subs: [], timer: null, last: null, lastAt: 0, busy: false };
      }
      var wanted = Math.max(POLL_MIN_S, everyS || POLL_MIN_S);
      p.every = p.subs.length ? Math.min(p.every, wanted) : wanted;
      var sub = { listener: listener, visible: true, charge: typeof charge === "function" ? charge : null, prepaid: prepaid === true };
      p.subs.push(sub);
      function visibleSubs() { return p.subs.filter(function (s) { return s.visible; }); }
      function tick() {
        p.timer = null;
        var watching = visibleSubs();
        if (!watching.length) return;
        if (p.busy) { schedule(); return; }
        var payers = [];
        watching.forEach(function (s) {
          if (s.prepaid) { s.prepaid = false; payers.push(s); }
          else if (!s.charge || s.charge()) payers.push(s);
          else s.listener({ ok: false, error: fail("rate_limited", "this message's read budget is spent") });
        });
        if (!payers.length) { schedule(); return; }
        p.busy = true;
        ask(verb, args).then(function (a) { p.last = { ok: true, value: a }; }, function (e) { p.last = { ok: false, error: e }; })
          .then(function () {
            p.busy = false;
            p.lastAt = now();
            // Whoever paid hears the answer; so does a subscriber that joined while it was in flight
            // (its registration already paid, or it is the page's own and costs nothing).
            var late = p.subs.filter(function (s) { return s.visible && payers.indexOf(s) < 0 && (s.prepaid || !s.charge); });
            late.forEach(function (s) { s.prepaid = false; });
            payers.concat(late).forEach(function (s) { if (p.subs.indexOf(s) >= 0) s.listener(p.last); });
            schedule();
          });
      }
      function schedule() {
        if (p.timer === null && visibleSubs().length) p.timer = setT(tick, p.every * 1000);
      }
      sub.poke = function () {
        if (p.last && p.last.ok && now() - p.lastAt < p.every * 1000) {
          // A late joiner is served what the shared poll already has.
          var last = p.last;
          sub.prepaid = false;
          Promise.resolve().then(function () { listener(last); });
          schedule();
        } else if (!p.busy) {
          if (p.timer !== null) { clearT(p.timer); p.timer = null; }
          tick();
        }
      };
      sub.stop = function () {
        var i = p.subs.indexOf(sub);
        if (i >= 0) p.subs.splice(i, 1);
        if (!p.subs.length) {
          if (p.timer !== null) clearT(p.timer);
          delete pollers[key];
        }
      };
      sub.poke();
      return sub;
    }

    return { ask: ask, watch: watch, _stats: function () { return { inFlight: inFlight, polls: Object.keys(pollers).length }; } };
  }

  // ---- the forwarder: the only path from an embed to the page's live pipe ----

  function createForwarder(opts) {
    var pool = opts.pool;
    var grant = parseGrant(opts.grant);
    var roleOf = typeof opts.role === "function" ? opts.role : function () { return null; };
    var frame = opts.frame;
    var now = opts.now || Date.now;
    var stamps = [];
    var inFlight = 0;
    var subs = Object.create(null);
    var subCount = 0;
    var destroyed = false;

    function reply(msg) {
      msg.cs = "embed";
      msg.v = VERSION;
      var w = frame && frame.contentWindow;
      // The reply goes to the embed's window only. Its origin is opaque, so "*" is the only target.
      if (w && !destroyed) w.postMessage(msg, "*");
    }
    function deny(id, error) {
      reply({ t: "result", id: id, ok: false, error: { code: error.code || "failed", message: String(error.message || error.code) } });
    }
    // The embed's allowance: reads in the last minute, one-shot and recurring alike.
    function spend() {
      var t = now();
      while (stamps.length && t - stamps[0] >= WINDOW_MS) stamps.shift();
      if (stamps.length >= EMBED_CALLS_PER_MIN) return false;
      stamps.push(t);
      return true;
    }
    function limited() {
      if (inFlight >= EMBED_IN_FLIGHT) return true;
      return !spend();
    }
    function gate(verb) {
      // The role is read now, not remembered: a viewer never gets the pipe because of run: owner.
      if (!mayRun(grant, roleOf())) throw fail("not_declared", "live messages do not run for this session");
      if (KNOWN_UNAVAILABLE[verb]) throw fail("unavailable", verb + " is not available here");
      if (!Object.prototype.hasOwnProperty.call(QUERY_OF, verb)) throw fail("not_declared", verb + " is not granted to embeds");
      if (!grant || grant.queries.indexOf(QUERY_OF[verb]) < 0) throw fail("not_declared", verb + " is not granted to embeds");
    }

    function handle(event) {
      if (destroyed || !frame || !event || event.source !== frame.contentWindow) return false;
      var m = event.data;
      if (!isObject(m) || m.cs !== "embed" || m.v !== VERSION) return false;
      var size;
      try { size = JSON.stringify(m).length; } catch (e) { return true; }
      if (size > MAX_ENVELOPE_BYTES) return true;
      if (m.t === "hello") {
        reply({ t: "hello", verbs: grant && mayRun(grant, roleOf()) ? ["queryContext", "subscribeContext", "inbox"].filter(function (v) {
          return grant.queries.indexOf(QUERY_OF[v === "subscribeContext" ? "queryContext" : v]) >= 0;
        }) : [] });
        return true;
      }
      if (typeof m.id !== "number" || !isFinite(m.id)) return true;
      if (m.t === "unsub") {
        if (subs[m.id]) { subs[m.id].stop(); delete subs[m.id]; subCount--; }
        return true;
      }
      if (m.t !== "call" && m.t !== "sub") return true;
      var verb = m.t === "sub" && m.verb === "subscribeContext" ? "queryContext" : m.verb;
      var args;
      try {
        if (typeof verb !== "string") throw fail("invalid_args", "no verb");
        gate(verb);
        args = normalizeArgs(verb, m.args, grant);
        if (m.t === "sub" && (subs[m.id] || subCount >= EMBED_SUBS)) throw fail("rate_limited", "too many subscriptions");
        if (limited()) throw fail("rate_limited", "too many asks from this embed");
      } catch (e) {
        deny(m.id, e);
        return true;
      }
      if (m.t === "call") {
        inFlight++;
        pool.ask(verb, args).then(function (a) {
          inFlight--;
          reply({ t: "result", id: m.id, ok: true, result: filterAnswer(verb, a, grant) });
        }, function (e) { inFlight--; deny(m.id, e); });
        return true;
      }
      subCount++;
      subs[m.id] = pool.watch(verb, args, typeof m.everyS === "number" ? m.everyS : POLL_MIN_S, function (r) {
        if (r.ok) reply({ t: "update", id: m.id, ok: true, result: filterAnswer(verb, r.value, grant) });
        else reply({ t: "update", id: m.id, ok: false, error: { code: r.error.code || "failed", message: r.error.message } });
      }, spend, true);
      return true;
    }

    function setVisible(visible) {
      Object.keys(subs).forEach(function (id) {
        subs[id].visible = visible !== false;
        if (subs[id].visible) subs[id].poke();
      });
    }
    function destroy() {
      destroyed = true;
      Object.keys(subs).forEach(function (id) { subs[id].stop(); });
      subs = Object.create(null);
      subCount = 0;
    }
    return { handle: handle, setVisible: setVisible, destroy: destroy };
  }

  // ---- transports ----

  function directTransport(pool, grantValue, roleFn) {
    var grant = parseGrant(grantValue);
    function prepare(verb, args) {
      gateDirect(verb);
      return normalizeArgs(verb, args, grant);
    }
    function gateDirect(verb) {
      if (roleFn && !mayRun(grant, roleFn())) throw fail("not_declared", "live messages do not run for this session");
      if (KNOWN_UNAVAILABLE[verb]) throw fail("unavailable", verb + " is not available here");
      if (!Object.prototype.hasOwnProperty.call(QUERY_OF, verb) || !grant || grant.queries.indexOf(QUERY_OF[verb]) < 0) {
        throw fail("not_declared", verb + " is not granted to embeds");
      }
    }
    return {
      caps: function () { return grant ? ["queryContext", "subscribeContext", "inbox"] : []; },
      call: function (verb, args) {
        return Promise.resolve().then(function () {
          var a = prepare(verb, args);
          return pool.ask(verb, a).then(function (r) { return filterAnswer(verb, r, grant); });
        });
      },
      subscribe: function (verb, args, cb) {
        var a;
        try { a = prepare(verb, args); } catch (e) { Promise.resolve().then(function () { cb(null, e); }); return function () {}; }
        var sub = pool.watch(verb, a, POLL_MIN_S, function (r) {
          if (r.ok) cb(filterAnswer(verb, r.value, grant), null); else cb(null, r.error);
        });
        return sub.stop;
      },
    };
  }

  function nestedTransport(parentWindow, listenWindow) {
    var host = listenWindow || (typeof window !== "undefined" ? window : null);
    var next = 0;
    var pending = Object.create(null);
    var watchers = Object.create(null);
    var verbs = ["queryContext", "subscribeContext", "inbox"];
    function post(msg) {
      msg.cs = "embed";
      msg.v = VERSION;
      parentWindow.postMessage(msg, "*");
    }
    host.addEventListener("message", function (e) {
      var m = e.data;
      // Only the page that holds this frame may answer it.
      if (e.source !== parentWindow || !isObject(m) || m.cs !== "embed" || m.v !== VERSION) return;
      if (m.t === "hello" && Array.isArray(m.verbs)) { verbs = m.verbs.filter(function (v) { return typeof v === "string"; }); return; }
      var settle = function (target) {
        if (!target) return;
        if (m.ok) target.ok(m.result); else target.fail(fail(m.error && m.error.code ? m.error.code : "failed", m.error && m.error.message));
      };
      if (m.t === "result" && pending[m.id]) {
        var p = pending[m.id];
        delete pending[m.id];
        settle(p);
      } else if (m.t === "result" && watchers[m.id] && !m.ok) {
        // A subscription the forwarder refused.
        watchers[m.id](null, fail(m.error && m.error.code ? m.error.code : "failed", m.error && m.error.message));
      } else if (m.t === "update" && watchers[m.id]) {
        watchers[m.id](m.ok ? m.result : null, m.ok ? null : fail(m.error && m.error.code ? m.error.code : "failed", m.error && m.error.message));
      }
    });
    post({ t: "hello" });
    return {
      caps: function () { return verbs.slice(); },
      call: function (verb, args) {
        return new Promise(function (ok, bad) {
          var id = ++next;
          pending[id] = { ok: ok, fail: bad };
          post({ t: "call", id: id, verb: verb, args: args === undefined ? {} : args });
        });
      },
      subscribe: function (verb, args, cb) {
        var id = ++next;
        watchers[id] = cb;
        post({ t: "sub", id: id, verb: verb === "queryContext" ? "subscribeContext" : verb, args: args === undefined ? {} : args });
        return function () {
          if (!watchers[id]) return;
          delete watchers[id];
          post({ t: "unsub", id: id });
        };
      },
    };
  }

  // ---- the SDK an embed sees ----

  function createSdk(transport) {
    return {
      version: "embed-" + VERSION,
      has: function (name) { return name !== "openFile" && transport.caps().indexOf(name) >= 0; },
      ready: function () {},
      queryContext: function (q) { return transport.call("queryContext", q || {}); },
      subscribeContext: function (q, cb) { return transport.subscribe("queryContext", q || {}, cb); },
      inbox: function (a) { return transport.call("inbox", a || {}); },
      // Opening a file needs the Mac app; a hosted pipe says so.
      openFile: function () { return Promise.reject(fail("unavailable", "openFile is not available here")); },
    };
  }

  // ---- the sandboxed document ----

  var CSP = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'";

  function buildSrcdoc(body, opts) {
    opts = opts || {};
    var boot = "var CSEmbed=(" + csEmbedFactory.toString() + ")();window[\"codesense\"]=CSEmbed.createSdk(CSEmbed.nestedTransport(window.parent));";
    return "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"" + CSP + "\">" +
      (opts.head || "") + "<script>" + boot.replace(/<\/script/gi, "<\\/script") + "</script></head><body>" + body + "</body></html>";
  }

  var api = {
    VERSION: VERSION,
    parseGrant: parseGrant,
    mayRun: mayRun,
    normalizeArgs: normalizeArgs,
    filterAnswer: filterAnswer,
    createPool: createPool,
    createForwarder: createForwarder,
    directTransport: directTransport,
    nestedTransport: nestedTransport,
    createSdk: createSdk,
    buildSrcdoc: buildSrcdoc,
    CSP: CSP,
    source: "var CSEmbed=(" + csEmbedFactory.toString() + ")();",
  };
  return api;
})();
