// cs-live 4
// The CodeSense dashboard toolkit. Plain functions over the hosted
// page API (queries and streams): the page posts each request to its own
// window and the viewer answers from its parent window. No network calls of its
// own, and nothing but the page API: a page that posts the messages itself
// works the same. Live answers and streams come only in the hosted viewer of a
// published dashboard; anywhere else nothing answers (see CS.ready).
//
//   CS.ready()                       -> Promise {role, control}; rejects "no_shell" after 3 s
//   CS.role()                        -> "viewer" | "owner" | null (before the viewer said hello)
//   CS.query(name, args)             -> Promise of the answer message
//   CS.queryAll(name, args, opts)    -> Promise {rows, total, complete, truncated, returned}
//   CS.subscribe(name, args, every_s, cb) -> stop()
//   CS.subscribeAll(name, args, every_s, cb, opts) -> stop(); stop.loadMore()/refresh() return promises
//   CS.partialList(opts)             -> {el, loadMore(), refresh(), stop()}; binds subscribe handle
//   CS.boot({render, queries, key}) -> {host, call, subscribe, state, refresh, stop}; one entry for the app and a shared page (the host is the
//                                       bridge that is present, never script[src]); render(ctx) runs from cached answers, then on each change
//   CS.cachedQuery(name, args, {maxAgeMs, key}) -> {peek() -> {value, ageMs, state: cached|fresh|stale|none}, refresh(), onChange(cb)}
//   CS.visibleTotals(container, {rowSelector, idAttr, root}) -> storyTotals for the rows on screen only; CS.batchTotals(ids) for a fixed list
//   CS.section(el, {key, load, render, when, scope}) -> one bounded read when a <details> opens or a region is visible; it reads again only on Retry/Refresh or dataChanged({keys})
//   CS.budget({inFlight: 2}) -> page-wide read queue; CS.paced(query, opts); CS.value(v, {pending, unavailable, error}); CS.stateBanner; CS.partial
//   CS.keyedRegion(el, scope) -> repaint in place keeping focus and scroll; CS.keyed(container, items, opts)
//   CS.onPresence(cb)                -> stop(); cb({mac, mac_seen_at}) or cb({wid, watchers, controlling, mine})
//   CS.viewer(cb)                    -> stop(); cb({role, mac, mac_seen_at, updated_at, stale, display}) now (once the viewer has said hello) and on each change.
//                                       mac: "online" | "offline" | "unknown" | null (no live block); display: the signed-in viewer's own email, "owner" for the owner.
//                                       A hosted dashboard draws no bar by default (chrome: none): subscribing is also how a page says it shows offline and stale itself
//   CS.endOwnerAccess()              -> Promise; owner only. The viewer ends this browser's owner access on the server, then the page reloads as a viewer.
//                                       Rejects not_owner, needs_gesture (call it from a click), busy, failed, timeout
//   CS.watch(stream, args, handlers) -> {stop()}; the low-level stream watch
//   CS.listAgents(cb)                -> stop(); cb(rows, state, detail) from the terminals stream
//   CS.listWorkspaces(cb)            -> stop(); cb(workspaces, state, detail)
//   CS.inbox(cb)                     -> stop(); cb(messages, state, detail); bodies need live.fields.inbox: [text]
//   CS.media(messageId, sha256)      -> Promise {url, mime, bytes}; a cached image of a message in this team inbox
//                                       (live.fields.inbox: [media] and media in live.queries) as a data: URL
//                                       (the frame's CSP allows img-src data: only). Rejects not_approved (media or its inbox field not declared), not_found, not_cached,
//                                       too_large (over the Mac's 5 MB cap: show a card), invalid_args or the query's own error
//   CS.send(to, text, replyTo?)       -> Promise {status, messageId}; owner only, plain text, 1–2,000 characters
//   CS.markRead(messageIds)           -> Promise {marked}; owner only. Marks 1–200 messages to the human in this team's inbox read, as opening them in the app does.
//                                       Rejects not_allowed (viewer), invalid_args, not_found (an id that is not human mail in this team: nothing is marked) or rate_limited
//   CS.confirmHint()                 -> Promise; resolves once sent, with nothing about eligibility. Asks the viewer to show its own "Confirm it's you" pill
//                                       when this browser's signed-in account is the dashboard owner's; it never opens the sheet, and shows nothing to anyone else
//   CS.passing(reason)               -> true for an end or error that passes (closed, failed, busy,
//                                       rate_limited): a list (listAgents, inbox, listWorkspaces) reopens itself after 2, 4, 8,
//                                       then every 16 s, hidden or not; watchTerminal retries its own watch;
//                                       a page retrying a CS.watch sooner calls the old watch's stop() first
//   CS.watchTerminal(handle, element, opts) -> {dispose(), onState(cb), onFollow(cb), onResize(cb), onControl(cb),
//                                       onFit(cb), setFontSize(px), fontSize(), setFit(on), fitting(),
//                                       jumpToLatest(), size(), controlling()}
//                                       (it reopens itself after a passing end: no page retry needed)
//                                       Fit (the default; opts.fit: false turns it off): the font is sized so
//                                       the session's columns fill the box's width (6 to 18 px; no taller than
//                                       the box when that leaves 10 px or more, else the width wins and the
//                                       screen scrolls, newest line at the bottom), and rows the box has to spare show the scrollback
//                                       above the screen, newest line at the bottom. The session keeps its
//                                       own size: nothing reflows. setFontSize turns fit off; setFit(true)
//                                       turns it on again. onFit's cb({fontSize, fit}) hears each change.
//                                       opts.theme: a CodeSense terminal theme by name (github-dark, the default,
//                                       dracula, nord, catppuccin, solarized-dark), as the app draws its terminals.
//   CS.fitFont(o)                     -> the fitted font size for o {cols, rows, width, height, cellWidth,
//                                       cellHeight} (cell size per px of font), in half pixels, 6 to 18
//                                       (each on…(cb) returns a function that removes cb)
//   CS.sendInput(handle, text, opts)  -> Promise; keys typed into that terminal (opts.paste: a paste), in order.
//                                       Rejects too_large over 16 KB or when the queue is full (1 MB, 1600 messages).
//                                       Rejects "not_controlling" unless the owner took control of it in
//                                       the viewer's top bar ("Take control"); a page cannot take it itself.
//   CS.resize(handle, cols, rows)     -> Promise; asks the session for that size, the same rule.
//   CS.onControl(cb)                  -> stop(); cb({handle, on, reason}) when control of a terminal turns on or off,
//                                       cb({handle, on, error, lost}) when input was refused (any P15 input error)
//
// Owner control: only the dashboard's owner, signed in, on a dashboard
// whose manifest says live.control: owner, after a click on the viewer's own
// "Take control". Then watchTerminal takes keys (xterm) and, on a phone, shows a
// key bar (Esc, Tab, Ctrl, arrows, Enter and a line to send). Invited viewers
// never type. Refusals (any P15 input error) reach onControl's cb as
// {handle, on, error, lost}, or {handle, on, lost: true, interrupted: true} when the
// owner's Ctrl-C (or Ctrl-D, Ctrl-Z, Esc) dropped what was waiting, on being
// whether control is still on. sendInput and resize resolve once the input is
// sent or queued (behind a paste still being sent, or while the second's send
// window is full); a later refusal, or the end of
// control while it waited (lost: true), comes through onControl, as do a paste
// over 1 MB ({error: "too_large"}) and a take the viewer's bar asked for that was
// refused ({on: false, error, about: "control"}).
//
// A watch keeps running while the page is hidden: tabbing away and back never asks for anything again. Pass {pauseWhenHidden: true}
// to stop a watch while hidden (a phone in the background); showing then re-opens it with from_seq, which is a new request.
// Hiding the page ends control too.
// Reserved for later stories: CS.launch, CS.stop.
var CS = (function () {
  "use strict";
  var VERSION = 1;
  var HELLO_WAIT_MS = 3000;
  var QUERY_WAIT_MS = 20000;
  var next = 0;
  var answers = Object.create(null);
  var actions = Object.create(null);
  var streams = Object.create(null);
  var presenceCbs = [];
  var viewerCbs = [];
  var viewerState = null;
  var viewerSubscribed = false;
  // Each terminal by handle (the newest watchTerminal of it), and onControl callbacks.
  var terms = Object.create(null);
  var controlCbs = [];
  var shellCovered = 0;
  var viewportCbs = [];
  var hello = null;
  var helloWaiters = [];
  var started = Date.now();
  // Ends and errors that pass (the Mac could not serve for a moment): a watch ended so is
  // opened again when the page is shown, and pages may retry it themselves (CS.passing).
  var PASSING = { closed: true, failed: true, busy: true, rate_limited: true };
  // A list watch that ended for a passing reason opens again after these (the last repeats), whether the page is hidden or not.
  var LIST_REOPEN_MS = [2000, 4000, 8000, 16000];

  function send(msg) {
    msg.cs = "live";
    msg.v = VERSION;
    // To this page's own window: the viewer's frame script forwards it with its token.
    window.postMessage(msg, "*");
  }

  window.addEventListener("message", function (e) {
    var m = e.data;
    if (e.source !== window.parent || window.parent === window || !m || m.cs !== "live") return;
    if (m.op === "acted" && actions[m.aid]) { actions[m.aid](m); return; }
    if (m.op === "hello") {
      hello = { role: m.role === "owner" ? "owner" : "viewer", control: m.control === true };
      helloWaiters.splice(0).forEach(function (w) { w.ok(hello); });
      return;
    }
    // How much of this frame a phone's on-screen keyboard covers, measured by the viewer.
    if (m.op === "viewport") {
      shellCovered = typeof m.covered === "number" && m.covered >= 0 ? Math.min(m.covered, 2000) : 0;
      viewportCbs.slice().forEach(function (cb) { cb(); });
      return;
    }
    if (m.op === "viewer") {
      viewerState = { role: m.role === "owner" ? "owner" : "viewer", mac: m.mac === undefined ? null : m.mac, mac_seen_at: m.mac_seen_at === undefined ? null : m.mac_seen_at, updated_at: m.updated_at === undefined ? null : m.updated_at, stale: m.stale === true, display: typeof m.display === "string" ? m.display : null };
      viewerCbs.slice().forEach(function (cb) {
        try { cb(Object.assign({}, viewerState)); } catch (e) { /* subscriber owns its errors */ }
      });
      return;
    }
    if (m.op === "presence") {
      var presence = m.wid === undefined
        ? { mac: m.mac, mac_seen_at: m.mac_seen_at }
        : { wid: m.wid, watchers: m.watchers, controlling: m.controlling, mine: m.mine === true };
      presenceCbs.slice().forEach(function (cb) {
        // A page subscriber must not prevent other subscribers or the watch from updating.
        try { cb(Object.assign({}, presence)); } catch (e) { /* subscriber owns its errors */ }
      });
      if (m.wid === undefined) return;
    }
    if (m.wid !== undefined) {
      var w = streams[m.wid];
      if (w) w(m);
      return;
    }
    if (m.id !== undefined) {
      var a = answers[m.id];
      if (a) a(m);
    }
  });

  function ready() {
    if (hello) return Promise.resolve(hello);
    return new Promise(function (ok, fail) {
      var waiter = { ok: ok };
      helloWaiters.push(waiter);
      setTimeout(function () {
        var i = helloWaiters.indexOf(waiter);
        if (i >= 0) { helloWaiters.splice(i, 1); fail(new Error("no_shell")); }
      }, Math.max(0, HELLO_WAIT_MS - (Date.now() - started)));
    });
  }

  // A Mac with every query slot taken answers busy (mac_busy hosted), and a relay with every slot
  // taken answers rate_limited: the read is asked again after 1, 2, 4, 8 and 8 s (plus up to
  // 250 ms), then the error is the page's. queryAll keeps its own rate_limited backoff.
  var BUSY_RETRY_MS = [1000, 2000, 4000, 8000, 8000];
  var BUSY = /^(mac_)?busy\b/;
  var LIMITED = /^rate_limited\b/;
  function retrying(codes, ask, tries, signal) {
    tries = tries || 0;
    return ask().then(function (answer) {
      if (tries) csCore.noteBusy(false, signal);
      return answer;
    }, function (err) {
      if (!codes.test(String(err && err.message)) || tries >= BUSY_RETRY_MS.length || (signal && signal.cancelled)) {
        if (tries) csCore.noteBusy(false, signal);
        throw err;
      }
      // The page is told once that the Mac is busy (pages say "Waiting for the Mac"), and once when the read is over.
      if (!tries) csCore.noteBusy(true, signal);
      return new Promise(function (ok) {
        setTimeout(ok, BUSY_RETRY_MS[tries] + Math.floor(Math.random() * 250));
      }).then(function () {
        // A cancelled or timed-out read stops here: no ask escapes the budget.
        if (signal && signal.cancelled) throw err;
        return retrying(codes, ask, tries + 1, signal);
      });
    });
  }

  // storyTotals for any number of ids, as one answer: the ids go in as many asks as fit
  // (`fits(chunk)`), three at a time, and the answers merge. Reason indexes are renumbered into
  // one reasons list; ids an answer could not hold (rest) are asked again; source is cache and
  // answered_at the oldest when any part came from the cache.
  function storyTotalsWith(ask, fits, ids) {
    if (!Array.isArray(ids)) return Promise.reject(new Error("invalid_args"));
    var queue = [], chunk = [];
    ids.forEach(function (id) {
      if (chunk.length && !fits(chunk.concat([id]))) { queue.push(chunk); chunk = []; }
      chunk.push(id);
    });
    if (chunk.length) queue.push(chunk);
    var out = { stories: {}, reasons: [], pending: [], indexing: { done: 0, total: 0 } };
    var index = Object.create(null);
    var source = "live", answeredAt = null;
    function add(m) {
      var a = m.answer || {};
      Object.keys(a.stories || {}).forEach(function (id) {
        var story = a.stories[id];
        if (story && story.unavailable) {
          var coded = {};
          Object.keys(story.unavailable).forEach(function (field) {
            var text = (a.reasons || [])[story.unavailable[field]];
            if (typeof text !== "string") return;
            if (!(text in index)) { index[text] = out.reasons.length; out.reasons.push(text); }
            coded[field] = index[text];
          });
          story = Object.assign({}, story, { unavailable: coded });
        }
        out.stories[id] = story;
      });
      out.pending = out.pending.concat(a.pending || []);
      if (a.indexing) out.indexing = a.indexing;
      if (m.source === "cache") source = "cache";
      if (typeof m.answered_at === "number" && (answeredAt === null || m.answered_at < answeredAt)) answeredAt = m.answered_at;
      if (Array.isArray(a.rest) && a.rest.length) queue.push(a.rest);
    }
    function next() {
      if (!queue.length) {
        out.source = source;
        if (answeredAt !== null) out.answered_at = answeredAt;
        return Promise.resolve(out);
      }
      var round = queue.splice(0, 3);
      return Promise.all(round.map(function (part) { return ask(part); })).then(function (answers) {
        answers.forEach(add);
        return next();
      });
    }
    return next();
  }

  // A hosted ask's args are at most 2 KB (R23): a storyTotals ask holds the ids that fit 1900
  // bytes of JSON, about 120 to 150.
  var STORY_TOTALS_ARGS_BYTES = 1900;
  function storyTotalsFits(chunk) {
    return new TextEncoder().encode(JSON.stringify({ storyIds: chunk })).length <= STORY_TOTALS_ARGS_BYTES;
  }
  // `ask(part)` replaces the default read, so the page's read budget can carry each ask.
  function storyTotals(ids, ask) {
    return ready().then(function () {
      return storyTotalsWith(ask || function (part) { return publicQuery("storyTotals", { storyIds: part }); }, storyTotalsFits, ids);
    });
  }

  function wireQuery(name, args, signal) {
    return retrying(BUSY, function () {
      return new Promise(function (ok, fail) {
        var id = "q" + ++next;
        var timer = setTimeout(function () { delete answers[id]; fail(new Error("no_answer")); }, QUERY_WAIT_MS);
        answers[id] = function (m) {
          clearTimeout(timer);
          delete answers[id];
          if (m.ok) ok(m); else fail(new Error(m.error));
        };
        send({ op: "query", id: id, query: name, args: args || {} });
      });
    }, 0, signal);
  }

  // The wire read behind CS.query: a busy Mac and a full relay are both asked again (see retrying).
  function wirePublic(name, args, signal) {
    return retrying(LIMITED, function () { return wireQuery(name, args, signal); }, 0, signal);
  }
  // CS.query: every page read goes through the page's read budget (CS.budget), then the wire.
  function publicQuery(name, args) { return csCore.ask(name, args); }
  // One page of a paged read: through the budget (skipping the relay retry: queryAll keeps its own rate_limited backoff), with a box so
  // stopping the walk frees its slot.
  function pageQuery(name, args, box) { return csCore.ask(name, args, { paging: true, box: box }); }

  // Mutations are sent once. A timeout is an unknown outcome, never an automatic retry.
  function act(name, args) {
    return ready().then(function () {
      return new Promise(function (ok, fail) {
        var aid = "a" + ++next;
        var timer = setTimeout(function () { delete actions[aid]; fail(new Error("timeout")); }, QUERY_WAIT_MS);
        actions[aid] = function (m) {
          clearTimeout(timer); delete actions[aid];
          if (m.ok) ok(m.result); else fail(new Error(m.error));
        };
        send({op: "act", aid: aid, action: name, args: args || {}});
      });
    });
  }

  function subscribe(name, args, everyS, cb) {
    var id = "s" + ++next;
    answers[id] = cb;
    send({ op: "subscribe", id: id, query: name, args: args || {}, every_s: everyS });
    return function () { delete answers[id]; send({ op: "unsubscribe", id: id }); };
  }

  // Paging state belongs to one traversal owner; head updates never imply deletion.
  var QUERYALL_MAX_ROWS = 5000;
  var QUERYALL_LIMIT = 100;

  var PAGED_LISTS = {
    sessionUsage: { rows: "sessions", id: function (r) { return JSON.stringify([r.cli, r.sessionId]); }, cursor: { arg: "cursor", next: "nextCursor" }, total: "total" },
    queryContext: { rows: "rows", id: function (r) { return r.id != null ? r.id : r.path; }, cursor: { arg: "cursor", next: "nextCursor" }, total: "total" },
    storyChanges: { rows: "commits", id: "sha", cursor: { arg: "cursor", next: "nextCursor" }, total: "total" }
  };

  function pagingValue(answer, selector) {
    if (typeof selector === "function") return selector(answer);
    return selector.split(".").reduce(function (value, key) { return value == null ? undefined : value[key]; }, answer);
  }

  function pagingAnswer(m, shape) {
    if (!m || m.ok === false) throw new Error(m && m.error || "failed");
    var a = m.answer;
    if (!a || typeof a !== "object") throw new Error("failed");
    if (a.refused) throw new Error(String(a.refused));
    var rows = pagingValue(a, shape.rows);
    var out = Object.assign({}, a);
    // Remove the selected array from metadata, preserving other facts and nested siblings.
    if (typeof shape.rows === "string" && shape.rows !== "rows") {
      var path = shape.rows.split(".");
      var parent = out;
      for (var i = 0; i < path.length - 1 && parent[path[i]]; i++) {
        parent[path[i]] = Object.assign({}, parent[path[i]]);
        parent = parent[path[i]];
      }
      delete parent[path[path.length - 1]];
    } else if (typeof shape.rows === "function") {
      Object.keys(out).forEach(function (key) { if (out[key] === rows) delete out[key]; });
    }
    out.rows = Array.isArray(rows) ? rows : [];
    out.total = pagingValue(a, shape.total);
    out.nextCursor = pagingValue(a, shape.cursor.next);
    ["source", "answered_at", "server_time"].forEach(function (key) {
      if (m[key] !== undefined) out[key] = m[key];
    });
    return out;
  }

  // A walk whose cursor answers stale_cursor (its index snapshot is gone) starts over this often.
  var STALE_CURSOR_RESTARTS = 3;

  function pagingOwner(name, args, opts, notify) {
    opts = opts || {};
    var defaults = PAGED_LISTS[name];
    var shape = Object.assign({
      rows: function (a) { return Array.isArray(a.sessions) ? a.sessions : a.rows; },
      id: function (r) { return r.id != null ? r.id : r.path; },
      cursor: { arg: "cursor", next: "nextCursor" }, total: "total"
    }, defaults);
    ["rows", "id", "total"].forEach(function (key) {
      if (opts[key] !== undefined) shape[key] = opts[key];
    });
    shape.cursor = Object.assign({}, shape.cursor);
    ["arg", "next"].forEach(function (key) {
      if (opts.cursor && opts.cursor[key] !== undefined) shape.cursor[key] = opts.cursor[key];
    });
    // Snapshot nested filters too: later caller edits must not change a traversal.
    var base = JSON.parse(JSON.stringify(args || {}));
    var limit = opts.limit || base.limit || QUERYALL_LIMIT;
    var cap = Math.min(opts.maxRows || QUERYALL_MAX_ROWS, QUERYALL_MAX_ROWS);
    base.limit = limit;
    var visible = new Map();
    var fresh = null;
    var cursor = base[shape.cursor.arg] || null;
    var total = null;
    var metadata = {};
    var pending = null;
    var stopped = false;
    var loading = false;
    var error = null;
    var headError = null;
    var initialized = false;
    var revision = 0;
    var staleRestarted = false;
    var staleCursorRestarts = 0;
    var waitingForHead = false;
    var recoveries = 2;
    var lastWalk = Date.now();
    var queuedHead = null;
    var lastHeadBody = null;
    var cancels = new Set();
    var stopSub = null;
    var restMs = (opts.restEveryS || 60) * 1000;
    var paging = !!defaults || opts.rows !== undefined;

    function merge(target, rows, overwrite) {
      rows.forEach(function (row) {
        var id = pagingValue(row, shape.id);
        if (target.has(id)) { if (overwrite) target.set(id, row); }
        else if (target.size < cap) target.set(id, row);
      });
    }
    function complete(collection) { return total !== null && collection.size === total && !cursor; }
    function result() {
      return Object.assign({}, metadata, {
        rows: Array.from(visible.values()), total: total, returned: visible.size,
        complete: initialized && !waitingForHead && !fresh && complete(visible),
        truncated: !(initialized && !waitingForHead && !fresh && complete(visible)) &&
          (!!cursor || (fresh || visible).size >= cap || recoveries > 0),
        canLoadMore: !waitingForHead && !!cursor && (fresh || visible).size < cap,
        waitingForHead: waitingForHead,
        atCeiling: (fresh || visible).size >= cap,
        loading: loading, error: error || headError
      });
    }
    function emit() { if (!stopped && notify) notify(result()); }
    function summary(a) {
      if (typeof a.total === "number") total = a.total;
      Object.keys(a).forEach(function (k) {
        if (k !== shape.cursor.next && k !== shape.total && ["rows", "total", "nextCursor", "hasMore", "next_before", "returned", "complete", "truncated", "loading", "error"].indexOf(k) < 0) metadata[k] = a[k];
      });
    }
    function accept(a, stamp) {
      var newer = stamp === revision;
      if (newer) summary(a);
      merge(visible, a.rows, newer);
      if (fresh) merge(fresh, a.rows, newer);
      cursor = paging && a.nextCursor || null;
      initialized = true;
      error = null;
      if (fresh && newer && complete(fresh)) { visible = fresh; fresh = null; }
      // A fenced final manual page can finish coverage without replacing newer rows.
      if (opts.manual === true && !newer && complete(visible)) fresh = null;
    }
    function cancellable(start) {
      return new Promise(function (resolve, reject) {
        var cleanup = function () {};
        function cancel() { cleanup(); reject(new Error("stopped")); }
        cancels.add(cancel);
        start(function (value) { cancels.delete(cancel); resolve(value); }, function (err) { cancels.delete(cancel); reject(err); }, function (fn) { cleanup = fn; });
      });
    }
    function ask(pageArgs, retry) {
      if (stopped) return Promise.reject(new Error("stopped"));
      return cancellable(function (resolve, reject, cleanup) {
        // Stopping cancels the budgeted read, which frees its slot.
        var box = {};
        cleanup(function () { if (box.cancel) box.cancel(); });
        pageQuery(name, pageArgs, box).then(resolve, reject);
      }).then(function (m) { return pagingAnswer(m, shape); }).catch(function (err) {
        if (stopped) throw new Error("stopped");
        if (err.message !== "rate_limited" || retry >= 6) throw err;
        return cancellable(function (resolve, reject, cleanup) {
          var timer = setTimeout(resolve, 5000 * (retry + 1));
          cleanup(function () { clearTimeout(timer); });
        }).then(function () { return ask(pageArgs, retry + 1); });
      });
    }
    function begin(a) {
      queuedHead = null;
      fresh = new Map();
      cursor = null;
      recoveries = 2;
      staleRestarted = false;
      staleCursorRestarts = 0;
      lastWalk = Date.now();
      if (a) accept(a, revision);
    }
    function canContinue(auto) {
      var collection = fresh || visible;
      if (collection.size >= cap || !paging) return false;
      if (cursor) return true;
      if (auto && total !== null && collection.size !== total && recoveries > 0) {
        recoveries--;
        return true;
      }
      return false;
    }
    function drive(auto, forceFirst) {
      if (stopped) return Promise.reject(new Error("stopped"));
      if (pending) return pending;
      if (waitingForHead) return Promise.resolve(result());
      loading = true;
      error = null;
      emit();
      async function run() {
        var first = forceFirst;
        var stamp;
        while (!stopped) {
          // Reconcile before deciding whether to ask or capturing request state.
          if (queuedHead) { begin(queuedHead); first = false; }
          if (!first) {
            if (stamp !== undefined && (!auto || (!cursor && stamp !== revision))) break;
            if (!canContinue(auto)) break;
          }
          first = false;
          var pageArgs = Object.assign({}, base);
          delete pageArgs[shape.cursor.arg];
          if (cursor) pageArgs[shape.cursor.arg] = cursor;
          stamp = revision;
          try {
            var a = await ask(pageArgs, 0);
            if (stopped) throw new Error("stopped");
            accept(a, stamp);
            emit();
          } catch (err) {
            var staleCursor = /^stale_cursor\b/.test(err.message);
            if (!stopped && pageArgs[shape.cursor.arg] && staleCursor && staleCursorRestarts < STALE_CURSOR_RESTARTS) {
              // The snapshot this walk paged is gone: walk again from the head, silently,
              // and only that new walk replaces the rows (none of the old walk's are kept).
              staleCursorRestarts++;
              cursor = null;
              fresh = new Map();
              first = true;
              continue;
            }
            if (!stopped && pageArgs[shape.cursor.arg] && !staleCursor && !staleRestarted && /stale|cursor|query_invalid/i.test(err.message)) {
              staleRestarted = true;
              cursor = null;
              if (opts.manual === true) {
                waitingForHead = true;
                throw err;
              }
              // Keep collected rows for a moving-window recovery, with the same limit.
              first = true;
              continue;
            }
            throw err;
          }
        }
      }
      pending = run().then(function () {
        loading = false;
        pending = null;
        emit();
        return result();
      }, function (err) {
        loading = false;
        pending = null;
        if (!stopped) { error = err; emit(); }
        throw err;
      });
      return pending;
    }
    function background(promise) {
      // Errors were delivered by the owner; automatic work has no Promise caller.
      promise.catch(function () {});
    }
    function head(m) {
      if (stopped) return;
      var a;
      try { a = pagingAnswer(m, shape); }
      catch (err) { headError = err; loading = !!pending; emit(); return; }
      headError = null;
      var wasWaiting = waitingForHead;
      waitingForHead = false;
      if (wasWaiting) staleRestarted = false;
      var body = JSON.stringify(m.answer);
      if (initialized && body === lastHeadBody) {
        error = null;
        if (queuedHead) queuedHead = a;
        // Transport freshness can change while an old cached head body repeats.
        ["source", "answered_at", "server_time"].forEach(function (key) {
          if (a[key] !== undefined) metadata[key] = a[key];
        });
        if (opts.manual !== true && !pending && Date.now() - lastWalk >= restMs) {
          begin();
          background(drive(true, true));
          return;
        }
        if (!cursor && ((total !== null && visible.size < total) || (wasWaiting && total === null))) cursor = paging && a.nextCursor || null;
        if (opts.manual === true && !pending && complete(visible)) fresh = null;
        emit();
        if (opts.manual !== true && !pending && cursor && (fresh || visible).size < cap) background(drive(true, false));
        return;
      }
      lastHeadBody = body;
      revision++;
      if (opts.manual === true && initialized) {
        summary(a);
        merge(visible, a.rows, true);
        if (fresh) merge(fresh, a.rows, true);
        // Manual heads update coverage without reopening a completed traversal.
        if (!cursor && !pending && ((total !== null && visible.size < total) || (wasWaiting && total === null))) cursor = paging && a.nextCursor || null;
        if (!pending && complete(visible)) fresh = null;
        error = null;
        if (!pending) loading = false;
        emit();
        return;
      }
      var previous = total;
      var start = !initialized || (typeof a.total === "number" && a.total !== previous) || Date.now() - lastWalk >= restMs;
      // Rearm an exhausted incomplete traversal without refetching healthy histories.
      if (!start && !pending && !cursor && (fresh || !complete(visible))) start = true;
      if (start && !pending) begin(a);
      else {
        summary(a);
        merge(visible, a.rows, true);
        if (fresh) merge(fresh, a.rows, true);
        error = null;
        if (start || queuedHead) queuedHead = a;
      }
      if (!pending) loading = false;
      emit();
      if (!pending && opts.manual !== true && canContinue(false)) background(drive(true, false));
      else if (!pending && opts.manual !== true && start && total !== null && !complete(fresh || visible)) background(drive(true, false));
    }
    function refresh() {
      if (stopped) return Promise.reject(new Error("stopped"));
      if (pending) return pending.then(refresh);
      if (waitingForHead) return Promise.resolve(result());
      begin();
      return drive(opts.manual !== true, true);
    }
    function stop() {
      if (stopped) return;
      stopped = true;
      cancels.forEach(function (cancel) { cancel(); });
      cancels.clear();
      if (stopSub) stopSub();
    }
    stop.loadMore = function () {
      if (!initialized) return drive(false, true);
      return drive(false, false);
    };
    stop.refresh = refresh;
    return {
      query: function () { return drive(true, true); },
      subscribe: function (everyS) { loading = true; emit(); stopSub = subscribe(name, base, everyS, head); return stop; }
    };
  }

  function queryAll(name, args, opts) {
    opts = opts || {};
    return pagingOwner(name, args, opts, opts.onPage).query();
  }

  function subscribeAll(name, args, everyS, cb, opts) {
    return pagingOwner(name, args, opts, cb).subscribe(everyS);
  }

  // A small list component that consumes a queryAll promise or a subscribeAll
  // callback and shows the approved status row.
  var PARTIAL_LIST_STYLE = [
    ":where(.cs-partial-list) { display: flex; flex-direction: column; gap: var(--cs-space-2, 8px); color-scheme: var(--cs-color-scheme, inherit); }",
    ":where(.cs-partial-list > .cs-partial-list-rows) { display: flex; flex-direction: column; }",
    ":where(.cs-partial-list > .cs-partial-list-status) { display: flex; align-items: center; justify-content: space-between; gap: var(--cs-space-2, 8px); flex-wrap: wrap; min-width: 0; font-family: var(--cs-font-sans, system-ui, -apple-system, sans-serif); font-size: var(--cs-text-base, 13px); line-height: var(--cs-leading, 1.5); }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-text) { font-size: var(--cs-text-sm, 12px); color: var(--cs-muted-fg, light-dark(#636c76, #8b949e)); }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more) { box-sizing: border-box; flex: none; display: inline-flex; align-items: center; justify-content: center; gap: 6px; height: 22px; padding: 0 var(--cs-space-2, 8px); font: inherit; font-size: var(--cs-text-xs, 11px); font-weight: 500; line-height: 1; color: var(--cs-fg, light-dark(#1f2328, #e6edf3)); background: var(--cs-surface, light-dark(#f7f8fa, #262b32)); border: 1px solid var(--cs-border, light-dark(#d0d7de, #30363d)); border-radius: var(--cs-radius, 6px); cursor: pointer; white-space: nowrap; }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:hover) { background: var(--cs-hover, color-mix(in srgb, var(--cs-fg, light-dark(#1f2328, #e6edf3)) 6%, transparent)); }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:focus-visible) { outline: 2px solid var(--cs-accent, light-dark(#0969da, #58a6ff)); outline-offset: 1px; }",
    ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:disabled) { opacity: 0.5; cursor: default; }",
    ":where(.cs-partial-list > .cs-partial-list-status) > .cs-partial-list-more[hidden] { display: none; }"
  ].join("\n");

  function installPartialListStyle() {
    if (document.getElementById("cs-partial-list-style")) return;
    var style = document.createElement("style");
    style.id = "cs-partial-list-style";
    style.textContent = PARTIAL_LIST_STYLE;
    document.head.insertBefore(style, document.head.firstChild);
  }

  function partialList(opts) {
    opts = opts || {};
    var bound = opts.status && opts.button;
    var container = opts.container;
    if (!container && !bound) throw new TypeError("partialList: pass container or status and button");
    if (!bound && typeof opts.render !== "function") throw new TypeError("partialList: pass render(row) -> element");
    var root = bound ? container || opts.status.parentElement : document.createElement("div");
    var rowsBox = null;
    var textEl = bound ? opts.status : document.createElement("span");
    var moreBtn = bound ? opts.button : document.createElement("button");
    if (!bound) {
      installPartialListStyle();
      root.className = "cs-partial-list";
      rowsBox = document.createElement("div");
      rowsBox.className = "cs-partial-list-rows";
      textEl.className = "cs-partial-list-text cs-small cs-muted";
      moreBtn.type = "button";
      moreBtn.className = "cs-partial-list-more cs-btn is-sm";
      var statusRow = document.createElement("div");
      statusRow.className = "cs-partial-list-status cs-row";
      statusRow.appendChild(textEl);
      statusRow.appendChild(moreBtn);
      root.appendChild(rowsBox);
      root.appendChild(statusRow);
      container.appendChild(root);
    }
    textEl.setAttribute("aria-live", "polite");
    var current = { rows: [], total: null, complete: false, truncated: false, returned: 0, loading: true };
    var handle = null;
    var stopped = false;
    var busy = null;
    var renderedRows = null;
    var settleAction = null;

    function update() {
      if (stopped) return;
      if (rowsBox) {
        var text = JSON.stringify(current.rows);
        if (text !== renderedRows) {
          renderedRows = text;
          rowsBox.innerHTML = "";
          current.rows.forEach(function (row, i) { rowsBox.appendChild(opts.render(row, i)); });
        }
      }
      var msg;
      if (current.loading && current.returned === 0) msg = "Loading…";
      else if (current.complete) msg = "Showing all " + current.returned;
      else if (typeof current.total !== "number") msg = "Loaded " + current.returned + " · total unknown";
      else if (current.truncated) msg = "Showing " + current.returned + " of " + current.total;
      else msg = "Latest " + current.returned + " of " + current.total;
      if (current.atCeiling && !current.complete) msg += " · row limit reached";
      if (current.loading && current.returned > 0) msg += " · loading more…";
      if (current.waitingForHead) msg += " · Waiting for fresh data…";
      else if (current.error) msg += " · Could not load: " + String(current.error.message || current.error) + ". Try again.";
      textEl.textContent = msg;
      var available = current.canLoadMore === undefined ? current.truncated : current.canLoadMore;
      moreBtn.textContent = current.waitingForHead ? "Waiting…" : busy || current.loading ? "Loading…" : current.error ? "Retry" : "Load more";
      moreBtn.disabled = !!busy || current.loading || current.waitingForHead || (!available && !current.error);
      moreBtn.hidden = !current.error && (current.complete || !available);
    }
    function receive(r) {
      if (stopped) return;
      current = r;
      update();
      if (typeof opts.onResult === "function") opts.onResult(r);
    }
    function run(refresh) {
      if (stopped || current.waitingForHead) return Promise.resolve(current);
      if (busy) return busy;
      var method = handle && (refresh ? handle.refresh : handle.loadMore);
      if (typeof method !== "function") method = refresh ? opts.query : opts.loadMore || opts.query;
      if (typeof method !== "function") return Promise.resolve(current);
      // Queue invocation so the click is locked before a synchronous subscriber update.
      var stoppedAction = new Promise(function (resolve) { settleAction = resolve; });
      busy = Promise.race([stoppedAction, Promise.resolve().then(function () { if (!stopped) return method(); }).then(function (r) {
        if (r && r.rows) receive(r);
        return current;
      }).catch(function (err) {
        if (!stopped) receive(Object.assign({}, current, { loading: false, error: err }));
        return current;
      })]).finally(function () { busy = null; settleAction = null; update(); });
      update();
      return busy;
    }
    function click() { run(!!current.error && current.canLoadMore === false); }
    moreBtn.addEventListener("click", click);
    if (typeof opts.subscribe === "function") handle = opts.subscribe(receive);
    else if (typeof opts.query === "function") run(true);
    update();
    return {
      el: root,
      loadMore: function () { return run(false); },
      refresh: function () { return run(true); },
      stop: function () {
        if (stopped) return;
        stopped = true;
        moreBtn.removeEventListener("click", click);
        moreBtn.disabled = true;
        if (settleAction) settleAction(current);
        if (typeof handle === "function") handle();
      }
    };
  }

  function viewer(cb) {
    viewerCbs.push(cb);
    // Subscribing tells the viewer this page shows offline and stale itself, so the viewer keeps its own notices away.
    if (!viewerSubscribed) { viewerSubscribed = true; send({ op: "viewer_sub" }); }
    if (viewerState) { try { cb(Object.assign({}, viewerState)); } catch (e) { /* subscriber owns its errors */ } }
    return function () { viewerCbs = viewerCbs.filter(function (c) { return c !== cb; }); };
  }

  function endOwnerAccess() {
    return ready().then(function (h) {
      if (h.role !== "owner") throw new Error("not_owner");
      return new Promise(function (ok, fail) {
        var id = "o" + ++next;
        var timer = setTimeout(function () { delete answers[id]; fail(new Error("timeout")); }, QUERY_WAIT_MS);
        answers[id] = function (m) {
          clearTimeout(timer); delete answers[id];
          if (m.ok) ok(); else fail(new Error(m.error));
        };
        send({ op: "end_owner", id: id });
      });
    });
  }

  function onPresence(cb) {
    presenceCbs.push(cb);
    return function () { presenceCbs = presenceCbs.filter(function (c) { return c !== cb; }); };
  }

  // One stream watch. handlers: {state(s, detail), value(v), snapshot(vt, cols, rows),
  // data(text), resized(cols, rows), presence(watchers, controlling)}. States:
  // connecting, live, paused (mac_offline | reconnecting | hidden), ended (reason), error (code).
  function watch(stream, args, handlers, opts) {
    opts = opts || {};
    var h = handlers || {};
    var wid = null;
    var seq = null;
    var parts = [];
    var stopped = false;
    // The watch was answered: a later bad_request is about a repeated wid, not this watch.
    var opened = false;
    var hiddenStop = opts.pauseWhenHidden === true;
    var reopenPassing = opts.reopenPassing === true, reopenTimer = 0, reopenTries = 0;
    function state(s, detail) { if (h.state) h.state(s, detail); }
    function on(m) {
      if (typeof m.seq === "number") seq = m.seq;
      if (m.op === "watching" || m.op === "value" || m.op === "snapshot") { opened = true; reopenTries = 0; }
      switch (m.op) {
        case "watching": return state("live");
        case "value": state("live"); return h.value && h.value(m.value);
        case "snapshot":
          if (m.part === 0) parts = [];
          parts.push(m.vt);
          if (m.last) { var vt = parts.join(""); parts = []; state("live"); if (h.snapshot) h.snapshot(vt, m.cols, m.rows); }
          return;
        case "data": return h.data && h.data(m.data);
        case "resized": return h.resized && h.resized(m.cols, m.rows);
        case "gap": parts = []; return send({ op: "resync", wid: wid });
        case "paused": return state("paused", m.reason);
        case "presence": return h.presence && h.presence(m.watchers, m.controlling, m.mine === true);
        // Control of this watch turned on or off (the viewer's top bar, or its end).
        case "control": return h.control && h.control(m.on === true, m.reason);
        // An ended or refused watch is over, unless it ended for a passing reason (CS.passing): a list opens that again by backoff
        // (opts.reopenPassing), hidden or not; showing the page never does. bad_request (a wid already watching) leaves the open watch going.
        case "ended": if (PASSING[m.reason]) passed(); else finish(); return state("ended", m.reason);
        case "error":
          // A refused input or control is not this watch's end.
          if (m.about === "input" || m.about === "control") return h.refused && h.refused(m.error, m.about, m.source);
          if (m.error === "bad_request" && opened) return; if (PASSING[m.error]) passed(); else finish(); return state("error", m.error);
      }
    }
    function close() {
      if (wid !== null) delete streams[wid];
      wid = null;
    }
    function passed() {
      close();
      if (!reopenPassing || stopped || reopenTimer) return;
      reopenTimer = setTimeout(function () { reopenTimer = 0; open(); }, LIST_REOPEN_MS[Math.min(reopenTries++, LIST_REOPEN_MS.length - 1)]);
    }
    function finish() {
      close();
      stopped = true;
      clearTimeout(reopenTimer);
      if (hiddenStop) document.removeEventListener("visibilitychange", visibility);
    }
    function open() {
      if (stopped || wid !== null) return;
      wid = "w" + ++next;
      opened = false;
      streams[wid] = on;
      var m = { op: "watch", wid: wid, stream: stream, args: args || {} };
      // Resume after the last frame seen: the viewer replays what it still holds.
      if (seq !== null) m.from_seq = seq + 1;
      state("connecting");
      send(m);
    }
    function pause() {
      if (wid === null) return;
      // pause: the viewer keeps the stream's recent output a while, so the watch on return resumes from it.
      send({ op: "unwatch", wid: wid, pause: true });
      close();
      state("paused", "hidden");
    }
    function visibility() {
      if (document.visibilityState === "hidden") pause(); else open();
    }
    if (hiddenStop) document.addEventListener("visibilitychange", visibility);
    if (!hiddenStop || document.visibilityState !== "hidden") open();
    else state("paused", "hidden");
    return {
      // The watch's current id (null while paused or ended): what input for it names.
      wid: function () { return wid; },
      stop: function () {
        if (stopped) return;
        if (wid !== null) send({ op: "unwatch", wid: wid });
        close();
        stopped = true;
        clearTimeout(reopenTimer);
        if (hiddenStop) document.removeEventListener("visibilitychange", visibility);
      },
    };
  }

  function listOf(stream, key) {
    return function (cb) {
      var last = [];
      var got = false;
      var w = watch(stream, {}, {
        value: function (v) { got = true; last = (v && Array.isArray(v[key])) ? v[key] : []; cb(last, "live"); },
        // live after a resume with nothing new still says so (the list is current again).
        state: function (s, detail) { if (s !== "live" || got) cb(last, s, detail); },
      }, { reopenPassing: true });
      return w.stop;
    };
  }

  // One image at a time, one chunk at a time, so images never starve the inbox stream or one
  // another of the viewer's 60 asks a minute; a rate_limited answer waits and asks again.
  var MEDIA_RETRIES = 6;
  var mediaTail = Promise.resolve();

  function mediaChunk(messageId, sha256, offset, tries) {
    return wireQuery("media", { messageId: messageId, sha256: sha256, offset: offset }).then(function (m) {
      var a = m.answer;
      if (!a || typeof a !== "object") throw new Error("failed");
      if (a.refused) throw new Error(String(a.refused));
      return a;
    }, function (e) {
      // The relay says query_not_declared when the media query, or the inbox media field the Mac
      // checks, is not declared: one reason to the page.
      if (e && e.message === "query_not_declared") throw new Error("not_approved");
      if (e && e.message === "rate_limited" && tries < MEDIA_RETRIES) {
        return new Promise(function (ok) { setTimeout(ok, 5000 * (tries + 1)); }).then(function () {
          return mediaChunk(messageId, sha256, offset, tries + 1);
        });
      }
      throw e;
    });
  }

  var MEDIA_MIMES = ["image/gif", "image/webp", "image/png", "image/jpeg"];

  // Join the decoded chunks into one buffer and encode it once, in slices (a multiple of 3 bytes so
  // no slice pads), so a 5 MB image never builds a long string a character at a time.
  function base64Of(parts, total) {
    var all = new Uint8Array(total);
    var at = 0;
    for (var i = 0; i < parts.length; i++) { all.set(parts[i], at); at += parts[i].length; }
    var out = [];
    for (var j = 0; j < total; j += 0x6000) {
      out.push(btoa(String.fromCharCode.apply(null, all.subarray(j, Math.min(j + 0x6000, total)))));
    }
    return out.join("");
  }

  function loadMedia(messageId, sha256) {
    var parts = [];
    var mime = null;
    var total = null;
    var offset = 0;
    function step() {
      return mediaChunk(messageId, sha256, offset, 0).then(function (a) {
        if (typeof a.data !== "string" || a.offset !== offset || !(a.length > 0)
          || (total !== null && a.total !== total)) throw new Error("failed");
        var bin = atob(a.data);
        var bytes = new Uint8Array(bin.length);
        for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        if (bytes.length !== a.length) throw new Error("failed");
        parts.push(bytes);
        if (mime !== null && a.mime !== mime) throw new Error("failed");
        mime = a.mime;
        total = a.total;
        offset += a.length;
        if (!a.done) return step();
        if (offset !== total) throw new Error("failed");
        if (MEDIA_MIMES.indexOf(mime) === -1) throw new Error("failed");
        return { url: "data:" + mime + ";base64," + base64Of(parts, total), mime: mime, bytes: total };
      });
    }
    return step();
  }

  function media(messageId, sha256) {
    if (!Number.isSafeInteger(messageId) || messageId <= 0 || typeof sha256 !== "string" || !/^[0-9a-f]{64}$/.test(sha256)) {
      return Promise.reject(new Error("invalid_args"));
    }
    var run = mediaTail.then(function () { return ready(); }).then(function () { return loadMedia(messageId, sha256); });
    mediaTail = run.then(function () {}, function () {});
    return run;
  }

  function sendMessage(to, text, replyTo) {
    return ready().then(function () {
      if (!hello || hello.role !== "owner") throw new Error("not_allowed");
      if (typeof to !== "string" || !to || typeof text !== "string" || !text.trim()
        || Array.from(text).length > 2000 || text.indexOf("\0") !== -1
        || (replyTo !== undefined && (!Number.isSafeInteger(replyTo) || replyTo <= 0))) throw new Error("invalid_args");
      var args = {to: to, text: text};
      if (replyTo !== undefined) args.replyTo = replyTo;
      return CS.act("ownerMessage", args);
    });
  }

  // The quick six the Mac accepts for ownerReact; anything else is refused there too.
  var REACTIONS = ["\uD83D\uDE02", "\u2764\uFE0F", "\uD83D\uDC4D", "\uD83D\uDE2E", "\uD83C\uDF89", "\uD83D\uDD25"];
  function react(messageId, emoji, remove) {
    return ready().then(function () {
      if (!hello || hello.role !== "owner") throw new Error("not_allowed");
      if (!Number.isSafeInteger(messageId) || messageId <= 0 || REACTIONS.indexOf(emoji) === -1
        || (remove !== undefined && typeof remove !== "boolean")) throw new Error("invalid_args");
      var args = {messageId: messageId, emoji: emoji};
      if (remove !== undefined) args.remove = remove;
      return CS.act("ownerReact", args).then(function (r) { return {on: !!(r && r.on)}; });
    });
  }

  // Hints that the page wants owner powers: the viewer reveals its own pill for the owner's account, else nothing. No answer comes back.
  function confirmHint() {
    return ready().then(function () { send({ op: "confirm_hint" }); });
  }

  // Owner only: mark messages to the human read, as opening them in the app does. At most 200 ids a call.
  function markRead(messageIds) {
    return ready().then(function () {
      if (!hello || hello.role !== "owner") throw new Error("not_allowed");
      if (!Array.isArray(messageIds) || messageIds.length < 1 || messageIds.length > 200
        || !messageIds.every(function (id) { return Number.isSafeInteger(id) && id > 0; })) throw new Error("invalid_args");
      return CS.act("ownerMarkRead", {messageIds: messageIds}).then(function (r) { return {marked: (r && Number.isSafeInteger(r.marked)) ? r.marked : 0}; });
    });
  }

  // The CodeSense app's terminal themes (src/features/viewer/components/terminal-constants.ts, TERMINAL_THEMES;
  // a test keeps the two the same), each with its chrome colour. opts.theme picks one by name; the app's
  // default, github-dark, otherwise. Names only: a page cannot pass colours or fonts of its own.
  // cs-themes:begin
  var THEMES = {"github-dark":{"chrome":"#161b22","background":"#0d1117","foreground":"#e6edf3","cursor":"#e6edf3","selectionBackground":"#264f78","black":"#484f58","red":"#ff7b72","green":"#3fb950","yellow":"#d29922","blue":"#58a6ff","magenta":"#bc8cff","cyan":"#39d353","white":"#b1bac4","brightBlack":"#6e7681","brightRed":"#ffa198","brightGreen":"#56d364","brightYellow":"#e3b341","brightBlue":"#79c0ff","brightMagenta":"#d2a8ff","brightCyan":"#56d364","brightWhite":"#f0f6fc"},"dracula":{"chrome":"#343746","background":"#282a36","foreground":"#f8f8f2","cursor":"#f8f8f2","selectionBackground":"#44475a","black":"#21222c","red":"#ff5555","green":"#50fa7b","yellow":"#f1fa8c","blue":"#bd93f9","magenta":"#ff79c6","cyan":"#8be9fd","white":"#f8f8f2","brightBlack":"#6272a4","brightRed":"#ff6e6e","brightGreen":"#69ff94","brightYellow":"#ffffa5","brightBlue":"#d6acff","brightMagenta":"#ff92df","brightCyan":"#a4ffff","brightWhite":"#ffffff"},"nord":{"chrome":"#3b4252","background":"#2e3440","foreground":"#d8dee9","cursor":"#d8dee9","selectionBackground":"#434c5e","black":"#3b4252","red":"#bf616a","green":"#a3be8c","yellow":"#ebcb8b","blue":"#81a1c1","magenta":"#b48ead","cyan":"#88c0d0","white":"#e5e9f0","brightBlack":"#7b88a1","brightRed":"#bf616a","brightGreen":"#a3be8c","brightYellow":"#ebcb8b","brightBlue":"#81a1c1","brightMagenta":"#b48ead","brightCyan":"#8fbcbb","brightWhite":"#eceff4"},"catppuccin":{"chrome":"#313244","background":"#1e1e2e","foreground":"#cdd6f4","cursor":"#f5e0dc","selectionBackground":"#9399b240","black":"#45475a","red":"#f38ba8","green":"#a6e3a1","yellow":"#f9e2af","blue":"#89b4fa","magenta":"#f5c2e7","cyan":"#94e2d5","white":"#bac2de","brightBlack":"#7f849c","brightRed":"#f38ba8","brightGreen":"#a6e3a1","brightYellow":"#f9e2af","brightBlue":"#89b4fa","brightMagenta":"#f5c2e7","brightCyan":"#94e2d5","brightWhite":"#a6adc8"},"solarized-dark":{"chrome":"#073642","background":"#002b36","foreground":"#93a1a1","cursor":"#93a1a1","selectionBackground":"#073642","black":"#073642","red":"#dc322f","green":"#859900","yellow":"#b58900","blue":"#268bd2","magenta":"#d33682","cyan":"#2aa198","white":"#eee8d5","brightBlack":"#60777e","brightRed":"#cb4b16","brightGreen":"#60777e","brightYellow":"#657b83","brightBlue":"#839496","brightMagenta":"#6c71c4","brightCyan":"#93a1a1","brightWhite":"#fdf6e3"}};
  // cs-themes:end
  var DEFAULT_THEME = "github-dark";
  function alpha(hex, opacity) {
    var rgb = parseInt(hex.slice(1), 16);
    return "rgba(" + ((rgb >> 16) & 255) + "," + ((rgb >> 8) & 255) + "," + (rgb & 255) + "," + opacity + ")";
  }
  function themeFor(name) {
    var t = Object.prototype.hasOwnProperty.call(THEMES, name) ? THEMES[name] : THEMES[DEFAULT_THEME];
    var out = {};
    for (var k in t) if (k !== "chrome") out[k] = t[k];
    out.cursorAccent = t.background;
    out.scrollbarSliderBackground = alpha(t.foreground, 0.10);
    out.scrollbarSliderHoverBackground = alpha(t.foreground, 0.18);
    out.scrollbarSliderActiveBackground = alpha(t.foreground, 0.25);
    // All terminal chrome derives from the selected palette, including author-facing CSS tokens.
    return { xterm: out, css: {
      bg: t.background, chrome: t.chrome, fg: t.foreground, muted: t.white,
      border: alpha(t.foreground, 0.18), "key-bg": alpha(t.foreground, 0.08),
      accent: t.blue, "accent-soft": alpha(t.blue, 0.14), "accent-border": alpha(t.blue, 0.45),
      warning: t.yellow, "warning-soft": alpha(t.yellow, 0.14), "pressed-fg": t.background,
      error: t.red, frame: alpha(t.foreground, 0.10), shadow: alpha(t.background, 0.35),
    } };
  }
  var FONT_MIN = 6;
  // 18 px at most: a narrow session (a Mac tab that is not on screen keeps a small size) is not blown up.
  var FONT_MAX = 18;
  var FONT_DEFAULT = 13;
  // The fit keeps the whole screen in the box's height when that leaves this size or more; else the width wins.
  var FONT_READABLE = 10;
  // At most this many scrollback rows are drawn above the screen to fill the box.
  var MAX_HISTORY = 300;
  var RETRY_FIRST_MS = 15000;
  var RETRY_MAX_MS = 120000;
  var MAX_COLS = 1000;
  var MAX_ROWS = 500;

  // What each end and error reads as in the terminal's note.
  var NOTES = {
    exited: "This session has ended. The last screen stays here.",
    not_found: "This session is not available to this dashboard any more.",
    not_declared: "This dashboard no longer shares terminals.",
    no_team: "This dashboard's folder is not in a team's context, so it shows no terminals.",
    access_ended: "Your access to this dashboard has ended.",
    replaced: "This dashboard is open in another window.",
    rate_limited: "Too many terminals are open right now; trying again shortly. Closing one helps.",
    no_xterm: "The terminal could not start: vendor/xterm.js is not loaded.",
    no_shell: "Live terminals work in the published dashboard.",
    failed: "The owner's Mac could not keep this terminal open; trying again shortly.",
    closed: "This terminal closed; trying again shortly.",
    busy: "The owner's Mac is watching too many terminals right now; trying again shortly.",
    invalid_args: "This terminal could not be opened (bad arguments).",
    too_large: "This terminal's screen is too large to send.",
  };
  // What a refused input reads as in the key bar's status line.
  // What the key bar's line says when it could not send (the viewer's own bar explains other refusals).
  var INPUT_NOTES = {
    too_large: "That is too much to send at once (16 KB at most).",
    not_controlling: "Take control in the top bar to type.",
  };
  var OTHERS_CONTROL = "The owner is controlling this terminal.";
  function tellControl(event) { controlCbs.slice().forEach(function (cb) { cb(event); }); }
  function noteFor(s, detail) {
    if (s === "paused") return detail === "mac_offline" ? "The owner's Mac is offline. The terminal goes on when it is back." : detail === "hidden" ? "" : "Reconnecting…";
    if (s === "ended" || s === "error") return NOTES[detail] || "This terminal stopped (" + detail + ").";
    return "";
  }
  // The toolkit's own look for its terminal: a scroll box, a note line and a Jump
  // button. Pages restyle the cs-term-* classes; these rules lose to any of theirs.
  // Native scroll anchoring would move the live prompt after the key bar changes the viewport.
  // follow() owns that position instead.
  var STYLE = ":where(.cs-term-wrap){position:relative;display:flex;flex-direction:column;flex:1;min-height:0;min-width:0}" +
    ":where(.cs-term){flex:1;min-height:0;min-width:0;overflow:auto;overflow-anchor:none;overscroll-behavior:contain;-webkit-overflow-scrolling:touch;background:var(--cs-term-bg);display:flex;flex-direction:column}" +
    ":where(.cs-term:focus-visible){outline:2px solid var(--cs-term-accent);outline-offset:-2px}" +
    // The screen sits at the bottom of the box, the scrollback rows above it; wider than the box, it scrolls sideways.
    ":where(.cs-term-host){display:block;flex:none;margin-top:auto;align-self:flex-start}" +
    ":where(.cs-term-wrap[data-narrow]) :where(.cs-term-host){align-self:center;padding:0 12px;box-shadow:0 0 0 1px var(--cs-term-frame);border-radius:6px}" +
    ":where(.cs-term-more){position:absolute;width:28px;pointer-events:none;background:linear-gradient(to right,transparent,var(--cs-term-bg))}" +
    ":where(.cs-term-note){margin:0;padding:10px 16px;font:14px/1.4 system-ui,sans-serif;color:var(--cs-term-fg);background:var(--cs-term-chrome)}" +
    ":where(.cs-term-jump){position:absolute;right:max(16px,env(safe-area-inset-right));bottom:max(16px,calc(12px + env(safe-area-inset-bottom)));min-height:44px;padding:0 16px;border-radius:999px;border:1px solid var(--cs-term-border);background:var(--cs-term-chrome);color:var(--cs-term-fg);font:600 14px system-ui,sans-serif;cursor:pointer}" +
    // The owner's key bar, above the on-screen keyboard while in control.
    ":where(.cs-keys){position:sticky;bottom:var(--cs-kb,0px);display:flex;flex-direction:column;gap:8px;padding:8px max(12px,env(safe-area-inset-right)) max(8px,env(safe-area-inset-bottom)) max(12px,env(safe-area-inset-left));background:var(--cs-term-chrome);border-top:1px solid var(--cs-term-border);font:14px/1.2 system-ui,sans-serif;color:var(--cs-term-fg)}" +
    ":where(.cs-keys-row){display:flex;gap:5px;overflow-x:auto;overscroll-behavior-x:contain;scrollbar-width:none;-webkit-overflow-scrolling:touch}" +
    ":where(.cs-keys-row)::-webkit-scrollbar{display:none}" +
    ":where(.cs-key){flex:none;display:inline-flex;align-items:center;justify-content:center;min-width:44px;min-height:44px;padding:0 8px;border:1px solid var(--cs-term-border);border-radius:8px;background:var(--cs-term-key-bg);color:var(--cs-term-fg);font:600 14px/1 system-ui,sans-serif;cursor:pointer;touch-action:manipulation;-webkit-user-select:none;user-select:none}" +
    ":where(.cs-key:active){background:var(--cs-term-border);transform:translateY(1px)}" +
    ":where(.cs-key[aria-pressed=true]){background:var(--cs-term-warning);border-color:var(--cs-term-warning);color:var(--cs-term-pressed-fg)}" +
    ":where(.cs-key:focus-visible,.cs-line input:focus-visible){outline:2px solid var(--cs-term-accent);outline-offset:2px}" +
    ":where(.cs-key svg){width:18px;height:18px}" +
    ":where(.cs-line){display:flex;min-width:0;gap:6px;margin:0}" +
    ":where(.cs-line input){flex:1;min-width:0;min-height:44px;padding:0 12px;border:1px solid var(--cs-term-border);border-radius:8px;background:var(--cs-term-bg);color:var(--cs-term-fg);font:16px/1.2 ui-monospace,SFMono-Regular,Menlo,monospace;caret-color:var(--cs-term-warning)}" +
    ":where(.cs-line input)::placeholder{color:var(--cs-term-muted)}" +
    ":where(.cs-keys-note){margin:0;font-size:13px;color:var(--cs-term-warning)}" +
    // A phone on its side: the keys and the line share one row, so the terminal keeps its height.
    "@media (max-height:500px){:where(.cs-keys){flex-direction:row;flex-wrap:wrap;align-items:center}:where(.cs-keys-row){flex:0 1 auto;min-width:0}:where(.cs-line){flex:1 1 200px}:where(.cs-keys-note){flex-basis:100%}}" +
    "@media (prefers-reduced-motion:reduce){:where(.cs-key:active){transform:none}}";
  var styled = false;
  function addStyle() {
    if (styled) return;
    styled = true;
    var tag = document.createElement("style");
    tag.textContent = STYLE;
    (document.head || document.documentElement).insertBefore(tag, (document.head || document.documentElement).firstChild);
  }

  // A read-only terminal in `element`, drawn by xterm.js (vendor/xterm.js, listed
  // in the dashboard). It renders at the session's own columns and rows (it never
  // reflows), with the font fitted to the box's width (see fit below), and the
  // rows the box has to spare filled with the scrollback above the screen; past
  // the smallest font it scrolls inside its box (horizontal scroll stays in the
  // terminal, never the page); it follows new output until the viewer scrolls up, and then shows
  // "Jump to latest". Paused, ended and error states show in a note under it; a
  // passing end (CS.passing) is opened again after 15 s, then longer, up to 2 min; the watch keeps
  // running while the page is hidden (tabbing away and back asks for nothing). With
  // no viewer to answer (the CodeSense viewer on a Mac) the note says so after 3 s.
  // opts: {fit} (default true), {fontSize} (6 to 18, default 13: the size while fit is off,
  // and until the first fit). Keys typed into it send nothing.
  function watchTerminal(handle, element, opts) {
    opts = opts || {};
    var stateCbs = [];
    var jumpCbs = [];
    var resizeCbs = [];
    var controlTermCbs = [];
    var fitCbs = [];
    var current = { state: "connecting" };
    // This terminal is under the owner's control here; or someone else (another window of the owner) controls it.
    var ctlOn = false;
    var othersControl = false;
    var fallbackFont = clampFont(opts.fontSize || FONT_DEFAULT);
    addStyle();
    var wrap = document.createElement("div");
    wrap.className = "cs-term-wrap";
    var theme = themeFor(opts.theme);
    Object.keys(theme.css).forEach(function (key) { wrap.style.setProperty("--cs-term-" + key, theme.css[key]); });
    var box = document.createElement("div");
    box.className = "cs-term";
    // The scroll box takes focus, so a keyboard can scroll the scrollback and the columns.
    box.tabIndex = 0;
    box.setAttribute("role", "region");
    box.setAttribute("aria-label", "Terminal output, read only");
    var host = document.createElement("div");
    host.className = "cs-term-host";
    var note = document.createElement("p");
    note.className = "cs-term-note";
    note.setAttribute("role", "status");
    note.hidden = true;
    var jump = document.createElement("button");
    jump.type = "button";
    jump.className = "cs-term-jump";
    jump.textContent = "Jump to latest";
    jump.hidden = true;
    box.appendChild(host);
    wrap.appendChild(box);
    // A fade at the box's right edge while columns are scrolled out of view there (a phone at the smallest font).
    var more = document.createElement("div");
    more.className = "cs-term-more";
    more.setAttribute("aria-hidden", "true");
    more.hidden = true;
    wrap.appendChild(more);
    wrap.appendChild(jump);
    wrap.appendChild(note);
    element.appendChild(wrap);
    function setState(s, detail) {
      current = { state: s, detail: detail };
      var text = noteFor(s, detail) || (othersControl && s === "live" ? OTHERS_CONTROL : "");
      note.textContent = text;
      note.hidden = !text;
      note.setAttribute("data-state", s);
      stateCbs.slice().forEach(function (cb) { cb(s, detail); });
    }
    function off(list, cb) { return function () { var i = list.indexOf(cb); if (i >= 0) list.splice(i, 1); }; }
    var api = {
      dispose: function () {},
      onState: function (cb) { stateCbs.push(cb); cb(current.state, current.detail); return off(stateCbs, cb); },
      onFollow: function (cb) { jumpCbs.push(cb); return off(jumpCbs, cb); },
      onResize: function (cb) { resizeCbs.push(cb); return off(resizeCbs, cb); },
      onControl: function (cb) { controlTermCbs.push(cb); return off(controlTermCbs, cb); },
      onFit: function (cb) { fitCbs.push(cb); return off(fitCbs, cb); },
      controlling: function () { return ctlOn; },
      setFontSize: function (n) { fallbackFont = clampFont(n); return fallbackFont; },
      fontSize: function () { return fallbackFont; },
      setFit: function () { return false; },
      fitting: function () { return false; },
      jumpToLatest: function () {},
      size: function () { return null; },
    };
    if (typeof Terminal !== "function") {
      setTimeout(function () { setState("error", "no_xterm"); }, 0);
      api.dispose = function () { wrap.remove(); stateCbs = []; jumpCbs = []; resizeCbs = []; fitCbs = []; };
      return api;
    }
    var fontSize = clampFont(opts.fontSize || FONT_DEFAULT);
    // The app's terminal font (MesloLGS NF) where the viewer has it, then the platforms' own monospace.
    var FONT_FAMILY = '"MesloLGS NF", Menlo, ui-monospace, "SFMono-Regular", "Cascadia Mono", Consolas, "Liberation Mono", monospace';
    // The scrollback rows above the screen: a second xterm, drawn from the screen's own buffer (see paintHistory).
    var histHost = document.createElement("div");
    histHost.className = "cs-term-history";
    histHost.setAttribute("aria-hidden", "true");
    histHost.hidden = true;
    var mainHost = document.createElement("div");
    mainHost.className = "cs-term-screen";
    host.appendChild(histHost);
    host.appendChild(mainHost);
    var term = new Terminal({
      cols: 80, rows: 24, fontSize: fontSize, disableStdin: true, cursorBlink: false, cursorInactiveStyle: "none",
      scrollback: 5000, drawBoldTextInBrightColors: false, allowProposedApi: false, theme: theme.xterm, fontFamily: FONT_FAMILY,
    });
    term.open(mainHost);
    var hist = new Terminal({
      cols: 80, rows: 1, fontSize: fontSize, disableStdin: true, cursorBlink: false, cursorInactiveStyle: "none",
      scrollback: 0, drawBoldTextInBrightColors: false, allowProposedApi: false, theme: theme.xterm, fontFamily: FONT_FAMILY,
    });
    hist.open(histHost);
    hist.write("\x1b[?25l");
    var histInput = histHost.querySelector("textarea");
    if (histInput) {
      histInput.setAttribute("readonly", "");
      histInput.setAttribute("tabindex", "-1");
      histInput.setAttribute("aria-hidden", "true");
      // xterm focuses its hidden textarea on a click even with tabindex=-1.
      // History is part of the same terminal: only its live screen accepts owner input.
      histInput.addEventListener("focus", function () {
        if (ctlOn) term.focus(); else histInput.blur();
      });
    }
    // Viewers are read-only: the hidden input xterm types into is taken out, and never
    // keeps focus, so no input cursor shows (the inactive cursor is not drawn).
    var input = mainHost.querySelector("textarea");
    function lockInput(locked) {
      if (!input) return;
      if (locked) {
        input.setAttribute("readonly", "");
        input.setAttribute("tabindex", "-1");
        input.setAttribute("aria-hidden", "true");
        if (document.activeElement === input) input.blur();
      } else {
        input.removeAttribute("readonly");
        input.removeAttribute("tabindex");
        input.removeAttribute("aria-hidden");
      }
    }
    lockInput(true);
    if (input) input.addEventListener("focus", function () { if (!ctlOn) input.blur(); });
    // History owns its selection, while keyboard focus stays on the live terminal
    // (or the document for a viewer). Bridge native Copy without sending a key.
    var macCopy = /Mac|iPhone|iPad|iPod/.test(navigator.platform);
    term.attachCustomKeyEventHandler(function (e) {
      var copy = e.type === "keydown" && e.key.toLowerCase() === "c" && !e.altKey && !e.shiftKey &&
        (macCopy ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey);
      return !(copy && hist.hasSelection());
    });
    function copyHistory(e) {
      var active = document.activeElement;
      if (!hist.hasSelection() || !e.clipboardData ||
          (active !== document.body && active !== input && active !== histInput && active !== box)) return;
      if (window.getSelection && window.getSelection().toString()) return;
      e.clipboardData.setData("text/plain", hist.getSelection());
      e.preventDefault();
      e.stopPropagation();
    }
    function clearHistorySelection(e) { if (!histHost.contains(e.target)) hist.clearSelection(); }
    document.addEventListener("copy", copyHistory, true);
    document.addEventListener("pointerdown", clearHistorySelection, true);
    term.onSelectionChange(function () { if (term.hasSelection()) hist.clearSelection(); });
    hist.onSelectionChange(function () { if (hist.hasSelection()) term.clearSelection(); });
    var following = true;
    function showJump(away) { jump.hidden = !away; jumpCbs.forEach(function (cb) { cb(away); }); }
    function atBottom() {
      var b = term.buffer.active;
      var down = box.scrollHeight - box.clientHeight - box.scrollTop <= 2;
      return b.viewportY >= b.baseY && down;
    }
    function follow() { if (following) { term.scrollToBottom(); box.scrollTop = box.scrollHeight; } }
    function scrolled() {
      // A resize (including the shell's control bar) can clamp scrollTop before
      // layout follows the live prompt. It is not the viewer scrolling away.
      if (laying || box.clientWidth !== layoutWidth || box.clientHeight !== layoutHeight) return;
      var bottom = atBottom();
      if (bottom !== following) { following = bottom; showJump(!following); }
    }
    term.onScroll(function () { scrolled(); histDirty = true; schedulePaint(); });
    // Into or out of a full-screen program: the history shows what it covers, or the scrollback again.
    term.buffer.onBufferChange(function () { histDirty = true; schedulePaint(); });
    term.onResize(function (size) { histDirty = true; schedulePaint(); resizeCbs.forEach(function (cb) { cb(size.cols, size.rows); }); });
    box.addEventListener("scroll", function () { scrolled(); moreRight(); }, { passive: true });
    function moreRight() {
      var hiddenRight = box.scrollWidth - box.clientWidth - box.scrollLeft > 1;
      more.hidden = !hiddenRight;
      if (!hiddenRight) return;
      more.style.top = box.offsetTop + box.clientTop + "px";
      more.style.height = box.clientHeight + "px";
      more.style.left = box.offsetLeft + box.clientLeft + box.clientWidth - 28 + "px";
    }
    host.addEventListener("wheel", function () { setTimeout(scrolled, 0); }, { passive: true });
    // A passing end or error (CS.passing) opens the terminal again a little later, waiting
    // longer each time up to two minutes, unless it came back by itself (a page show).
    var disposed = false;
    var retryMs = RETRY_FIRST_MS;
    var retryTimer = 0;
    function onState(s, detail) {
      if (s === "live") retryMs = RETRY_FIRST_MS;
      // The viewer ends control with the watch (paused, ended): the terminal stops taking keys at once.
      if (s !== "live" && ctlOn) setControl(false, s === "paused" ? detail : "ended");
      setState(s, detail);
      if ((s === "ended" || s === "error") && PASSING[detail] && !retryTimer) {
        retryTimer = setTimeout(function () {
          retryTimer = 0;
          // Only a terminal still ended or refused for a passing reason: not one that has come
          // back (live, connecting) or one that has since ended for good.
          if (disposed || !((current.state === "ended" || current.state === "error") && PASSING[current.detail])) return;
          w.stop();
          w = start();
        }, retryMs);
        retryMs = Math.min(retryMs * 2, RETRY_MAX_MS);
      }
    }
    function start() {
      return watch("terminal", { handle: handle }, {
        state: onState,
        snapshot: function (vt, cols, rows) {
          term.reset();
          histDirty = true;
          if (cols && rows) fit(cols, rows);
          cprTail = "";
          countQueries(vt);
          term.write(vt, function () { follow(); schedulePaint(); });
        },
        data: function (text) {
          countQueries(text);
          // Erasing scrollback (CSI 3 J) need not emit a scroll event. Refresh after
          // parsing too, so cleared or replaced history never lingers in the extra rows.
          term.write(text, function () { histDirty = true; follow(); schedulePaint(); });
        },
        resized: function (cols, rows) { fit(cols, rows); follow(); },
        control: function (on, reason) { onControlFrame(on, reason); },
        presence: function (watchers, controlling, mine) { onPresence(watchers, controlling, mine); },
        refused: function (error, about, source) { onRefused(error, about, source); },
      });
    }
    // ---- owner control ----
    var keys = null;
    var ctrlKey = null;
    var keysNote = null;
    var ctrlArmed = false;
    var noteTimer = 0;
    function sendNow(data, kind) {
      var wid = w && w.wid ? w.wid() : null;
      if (!ctlOn || wid === null || !data) return false;
      noteSent(utf8Length(data));
      send({ op: "input", wid: wid, kind: kind || "keys", data: data });
      return true;
    }
    // ---- The send queue ----
    // Every input goes in order: while anything waits, the next input waits behind it, and the queue sends as
    // fast as the send window allows. A long paste goes in pieces of at most 12 KB, each a P15 paste of its own
    // (the Mac strips markers and brackets each multi-line piece, so a piece cut short can never leave the
    // session inside a paste); pieces are cut after a line break where one is near (xterm sends a pasted line
    // break as CR alone). An interrupt (Ctrl-C, Ctrl-D, Ctrl-Z, Esc alone) does not wait: it drops what is still
    // waiting (the page hears lost: true) and goes at once. At most MAX_PASTE bytes and MAX_WAITING messages
    // wait, so nothing sits behind more than about 20 seconds of input; past that the new input is refused whole.
    var pending = [];
    var pendingTimer = 0;
    var PIECE = 12 * 1024;
    var MAX_PASTE = 1024 * 1024;
    var MAX_WAITING = 1600;
    var INTERRUPTS = { "\x03": true, "\x04": true, "\x1a": true, "\x1b": true };
    // K39 in the toolkit too: everything sent in any second stays under 48 KB and 80 messages (the relay and
    // the Mac allow 64 KB and 100), leaving room for delivery jitter. A refusal still drops what waits.
    var sentWindow = [];
    function noteSent(bytes) { sentWindow.push({ at: Date.now(), bytes: bytes }); }
    function windowRoom(cost) {
      var now = Date.now();
      while (sentWindow.length && now - sentWindow[0].at >= 1000) sentWindow.shift();
      var bytes = 0;
      for (var i = 0; i < sentWindow.length; i++) bytes += sentWindow[i].bytes;
      return sentWindow.length < 80 && bytes + cost <= 48 * 1024;
    }
    // The bytes waiting, kept as items are queued and sent (each item carries its own size).
    var pendingBytes = 0;
    function waitingBytes() { return pendingBytes; }
    function refuseFull(text) {
      flash(text);
      var full = { handle: handle, on: ctlOn, error: "too_large", lost: false };
      controlTermCbs.slice().forEach(function (cb) { cb(full); });
      tellControl(full);
      return "too_large";
    }
    function sendItem(item) {
      if (item.resize) {
        var wid = w && w.wid ? w.wid() : null;
        if (ctlOn && wid !== null) { noteSent(0); send({ op: "input", wid: wid, kind: "resize", cols: item.resize[0], rows: item.resize[1] }); }
        return;
      }
      sendNow(item.data, item.kind);
    }
    function dropPending() {
      var had = pending.length > 0;
      pending = [];
      pendingBytes = 0;
      if (pendingTimer) { clearTimeout(pendingTimer); pendingTimer = 0; }
      return had;
    }
    function pump() {
      pendingTimer = 0;
      if (!ctlOn) return dropPending();
      while (pending.length && windowRoom(pending[0].size)) {
        var item = pending.shift();
        pendingBytes -= item.size;
        sendItem(item);
      }
      if (pending.length) pendingTimer = setTimeout(pump, 100);
    }
    // Queues items (or sends at once when nothing waits and the window has room). Returns true, "too_large" when
    // refused by the caps, or false without control.
    function enqueue(items) {
      if (!ctlOn) return false;
      var size = 0;
      for (var i = 0; i < items.length; i++) {
        items[i].size = items[i].data ? utf8Length(items[i].data) : 0;
        size += items[i].size;
      }
      if (pending.length + items.length > MAX_WAITING || waitingBytes() + size > MAX_PASTE) {
        return refuseFull(size > MAX_PASTE ? "That paste is too long to type (1 MB at most). Nothing was sent." : "Too much is still waiting to be sent. Nothing more was queued.");
      }
      if (!pending.length && items.length === 1 && windowRoom(size)) { sendItem(items[0]); return true; }
      pending = pending.concat(items);
      pendingBytes += size;
      if (!pendingTimer) pump();
      return true;
    }
    // fromKeys: typed by the owner (xterm or the key bar). Only then does an interrupt jump the queue; a page's
    // sendInput always goes in order.
    function sendKeys(data, kind, fromKeys) {
      if (!ctlOn || !data) return false;
      // An interrupt goes first: whatever waits is dropped (and the page told), so Ctrl-C stops a long paste.
      if (fromKeys && INTERRUPTS[data] && (!kind || kind === "keys")) {
        if (dropPending()) {
          var lostEvent = { handle: handle, on: ctlOn, lost: true, interrupted: true };
          controlTermCbs.slice().forEach(function (cb) { cb(lostEvent); });
          tellControl(lostEvent);
        }
        return sendNow(data, kind);
      }
      return enqueue([{ data: data, kind: kind }]);
    }
    function sendLong(data, kind, fromKeys) {
      if (!data) return false;
      if (utf8Length(data) <= PIECE) return sendKeys(data, kind, fromKeys);
      return enqueue(chunks(data, PIECE).map(function (d) { return { data: d, kind: kind }; }));
    }
    // xterm wraps a paste in bracketed-paste markers when the session asked for them: it goes as P15 paste
    // pieces with those markers stripped, so pasted text can never end a paste early; the Mac strips again and
    // brackets each multi-line piece (the submit_line rule).
    var PASTE_START = "\x1b[200~";
    var PASTE_END = "\x1b[201~";
    var stripMarkers = stripMarkersAll;
    // Device status reports (CPR) look like modified F3 keys: one counts as a report only while the session's
    // own query (CSI 6n) is waiting for it.
    var cprWaiting = 0;
    // The end of the last output, so a query split across two frames is still seen.
    var cprTail = "";
    function countQueries(text) {
      if (!ctlOn) { cprTail = ""; return; }
      var seen = cprTail + text;
      cprWaiting = Math.min(8, cprWaiting + (seen.match(/\x1b\[\??6n/g) || []).length);
      var esc = seen.lastIndexOf("\x1b");
      // Keep an escape that may be the start of a query (at most "ESC [ ? 6" so far), never a whole one again.
      cprTail = esc >= 0 && seen.length - esc <= 4 && !/n/.test(seen.slice(esc)) ? seen.slice(esc) : "";
    }
    // Sticky Ctrl: the next character typed (xterm, the line field or a key) goes as its control code.
    function withCtrl(data) {
      if (!ctrlArmed || data.length !== 1) return data;
      setCtrl(false);
      var c = data.toUpperCase().charCodeAt(0);
      return c >= 64 && c <= 95 ? String.fromCharCode(c - 64) : c === 63 ? "\x7f" : data;
    }
    function setCtrl(on) {
      ctrlArmed = on;
      if (ctrlKey) ctrlKey.setAttribute("aria-pressed", on ? "true" : "false");
    }
    function flash(text) {
      // Without the key bar (a desktop), the terminal's own note line says it for a while.
      if (!keysNote) {
        if (!text || current.state !== "live") return;
        note.textContent = text;
        note.hidden = false;
        if (noteTimer) clearTimeout(noteTimer);
        noteTimer = setTimeout(function () { setState(current.state, current.detail); }, 6000);
        return;
      }
      keysNote.textContent = text || "";
      keysNote.hidden = !text;
      if (noteTimer) clearTimeout(noteTimer);
      if (text) noteTimer = setTimeout(function () { keysNote.hidden = true; }, 6000);
    }
    function arrow(code) {
      // With Ctrl armed: a Ctrl-arrow, as a hardware Ctrl+arrow sends it (CSI 1;5 x), and Ctrl is used up.
      if (ctrlArmed) { setCtrl(false); return "\x1b[1;5" + code; }
      // Application cursor keys (DECCKM), as the session asked for them.
      var app = term.modes && term.modes.applicationCursorKeysMode;
      return (app ? "\x1bO" : "\x1b[") + code;
    }
    var SVG = { left: "M15 6l-6 6 6 6", up: "M6 15l6-6 6 6", down: "M6 9l6 6 6-6", right: "M9 6l6 6-6 6" };
    function keyButton(label, aria, data) {
      var b = document.createElement("button");
      b.type = "button";
      b.className = "cs-key";
      if (SVG[label]) {
        b.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="' + SVG[label] + '"/></svg>';
      } else b.textContent = label;
      b.setAttribute("aria-label", aria);
      // The key keeps focus where it was, so a phone's keyboard stays open.
      b.addEventListener("pointerdown", function (e) { e.preventDefault(); });
      // Esc and Tab are not changed by Ctrl (and leave it armed for the next character).
      b.addEventListener("click", function () { if (typeof data === "function") data(); else sendKeys(data, "keys", true); });
      return b;
    }
    function wantKeyBar() {
      if (opts.keyBar === true) return true;
      if (opts.keyBar === false) return false;
      return !!(window.matchMedia && window.matchMedia("(pointer: coarse), (max-width: 720px)").matches);
    }
    function viewportGap() {
      var vv = window.visualViewport;
      var own = vv ? Math.max(0, window.innerHeight - vv.height - vv.offsetTop) : 0;
      var gap = Math.max(own, shellCovered);
      if (keys) keys.style.setProperty("--cs-kb", Math.round(gap) + "px");
      // The terminal ends above the keyboard and the key bar, so its newest line (at the bottom) stays in view:
      // the box shrinks by the keyboard's height and the fit lays it out again.
      wrap.style.paddingBottom = keys && !keys.hidden && gap > 0 ? Math.round(gap) + "px" : "";
    }
    function buildKeys() {
      keys = document.createElement("div");
      keys.className = "cs-keys";
      keys.setAttribute("role", "toolbar");
      keys.setAttribute("aria-label", "Terminal keys");
      var row = document.createElement("div");
      row.className = "cs-keys-row";
      row.appendChild(keyButton("Esc", "Escape", "\x1b"));
      row.appendChild(keyButton("Tab", "Tab", "\t"));
      ctrlKey = keyButton("Ctrl", "Control, applies to the next key", function () { setCtrl(!ctrlArmed); });
      ctrlKey.setAttribute("aria-pressed", "false");
      row.appendChild(ctrlKey);
      row.appendChild(keyButton("left", "Left arrow", function () { sendKeys(arrow("D"), "keys", true); }));
      row.appendChild(keyButton("up", "Up arrow", function () { sendKeys(arrow("A"), "keys", true); }));
      row.appendChild(keyButton("down", "Down arrow", function () { sendKeys(arrow("B"), "keys", true); }));
      row.appendChild(keyButton("right", "Right arrow", function () { sendKeys(arrow("C"), "keys", true); }));
      // Not a <form>: the page runs in a sandbox without allow-forms, where a submit never fires.
      var form = document.createElement("div");
      form.className = "cs-line";
      var line = document.createElement("input");
      line.type = "text";
      line.enterKeyHint = "send";
      line.autocomplete = "off";
      line.setAttribute("autocapitalize", "off");
      line.setAttribute("autocorrect", "off");
      line.spellcheck = false;
      line.placeholder = "Type a line, then Enter";
      line.setAttribute("aria-label", "Line to send to the terminal");
      // With Ctrl armed, the next character typed here goes as its control code (Ctrl-C and the like).
      line.addEventListener("beforeinput", function (e) {
        if (ctrlArmed && e.inputType === "insertText" && e.data && e.data.length === 1) {
          e.preventDefault();
          sendKeys(withCtrl(e.data), "keys", true);
        }
      });
      function sendLine() {
        // The line stays in the field when nothing could be sent, so nothing typed is lost.
        if (!ctlOn) return flash(INPUT_NOTES.not_controlling);
        if (utf8Length(line.value) > MAX_INPUT) return flash(INPUT_NOTES.too_large);
        // The line goes as a paste, then Enter as its own key (P15 paste, keys), queued together or not at all.
        var sent = line.value ? enqueue([{ data: line.value, kind: "paste" }, { data: "\r", kind: "keys" }]) : sendKeys("\r", "keys", true);
        if (sent !== true) return;
        line.value = "";
      }
      line.addEventListener("keydown", function (e) {
        if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); sendLine(); }
      });
      // Enter sits by the line: it sends the line typed there (if any), then Enter. Seven keys above
      // and this row both fit 375 px with 44 px targets.
      var sendBtn = keyButton("Enter", "Enter", sendLine);
      form.appendChild(line);
      form.appendChild(sendBtn);
      keysNote = document.createElement("p");
      keysNote.className = "cs-keys-note";
      keysNote.setAttribute("role", "status");
      keysNote.hidden = true;
      keys.appendChild(row);
      keys.appendChild(form);
      keys.appendChild(keysNote);
      wrap.insertBefore(keys, note);
      if (window.visualViewport) {
        window.visualViewport.addEventListener("resize", viewportGap);
        window.visualViewport.addEventListener("scroll", viewportGap);
      }
      viewportCbs.push(viewportGap);
      viewportGap();
    }
    function setControl(on, reason) {
      if (on === ctlOn) return;
      ctlOn = on;
      term.options.disableStdin = !on;
      lockInput(!on);
      box.setAttribute("aria-label", on ? "Terminal, you are controlling it" : "Terminal output, read only");
      if (on) wrap.setAttribute("data-controlling", "");
      else wrap.removeAttribute("data-controlling");
      if (on && wantKeyBar() && !keys) buildKeys();
      if (keys) { keys.hidden = !on; viewportGap(); }
      var lost = !on && pending.length > 0;
      if (!on) { setCtrl(false); flash(""); dropPending(); cprWaiting = 0; cprTail = ""; }
      if (on && !wantKeyBar()) term.focus();
      var event = { handle: handle, on: on, reason: reason };
      if (lost) event.lost = true;
      controlTermCbs.slice().forEach(function (cb) { cb(event); });
      tellControl(event);
    }
    // xterm answers some of the session's own queries (cursor position, device attributes, focus):
    // the owner's Mac terminal answers those already, so they are not sent as keys.
    term.onData(function (data) {
      if (!ctlOn) return;
      if (/^\x1b\[\??[\d;]*R$/.test(data) && cprWaiting > 0) { cprWaiting--; return; }
      if (isReport(data)) return;
      hist.clearSelection();
      if (data.indexOf(PASTE_START) === 0) {
        // Over the 1 MB cap: refused before any work on it.
        if (data.length > MAX_PASTE + 12) return refuseFull("That paste is too long to type (1 MB at most). Nothing was sent.");
        return sendLong(stripMarkers(data), "paste");
      }
      sendLong(withCtrl(data), "keys", true);
    });
    // sendInput text (at most K38) goes whole, in order behind any paste pieces.
    var self = { handle: handle, on: function () { return ctlOn; }, wid: function () { return w && w.wid ? w.wid() : null; }, send: function (data, kind) { return sendKeys(data, kind); }, resize: function (cols, rows) {
      // A resize waits in the queue too, so it lands in order.
      return enqueue([{ resize: [cols, rows] }]);
    } };
    terms[handle] = self;
    function onControlFrame(on, reason) { setControl(on, reason); }
    function onPresence(watchers, controlling, mine) {
      var others = controlling && !mine && !ctlOn;
      if (others === othersControl) return;
      othersControl = others;
      setState(current.state, current.detail);
    }
    function onRefused(error, about, source) {
      // The viewer's answer to the page's own control post (pages cannot take control) is not a refused take.
      if (about === "control" && source === "page") return;
      if (about === "control") {
        // A take the viewer asked for and the relay refused (the viewer's bar says why).
        var refused = { handle: handle, on: false, error: error, about: "control" };
        controlTermCbs.slice().forEach(function (cb) { cb(refused); });
        tellControl(refused);
        return;
      }
      if (about !== "input") return;
      var lost = pending.length > 0;
      dropPending();
      // The viewer's own bar already says why an input was refused; the key bar adds only what it alone knows.
      if (lost) flash("What was still waiting to be sent was dropped.");
      var event = { handle: handle, on: ctlOn, error: error, lost: lost };
      controlTermCbs.slice().forEach(function (cb) { cb(event); });
      tellControl(event);
    }
    var w = start();
    // The session's own size, within what a browser can draw (the relay allows up to 65535).
    function fit(cols, rows) {
      term.resize(Math.min(cols, MAX_COLS), Math.min(rows, MAX_ROWS));
      layout();
    }
    // ---- Fit: the font fills the box's width; spare rows show the scrollback ----
    var fitOn = opts.fit !== false;
    var screenEl = mainHost.querySelector(".xterm-screen");
    var lastFit = null;
    function tellFit() {
      var now = fontSize + "/" + fitOn;
      if (now === lastFit) return;
      lastFit = now;
      var event = { fontSize: fontSize, fit: fitOn };
      fitCbs.slice().forEach(function (cb) { cb(event); });
    }
    function setFont(n) {
      if (term.options.fontSize !== n) term.options.fontSize = n;
      if (hist.options.fontSize !== n) hist.options.fontSize = n;
      // The pinned xterm 6.0.0 Viewport does not observe font-only dimension changes.
      // Sync its scroll rail without resizing the buffer (which would reflow the session).
      // Keep this version-specific workaround guarded for future vendor upgrades.
      [term, hist].forEach(function (t) {
        var viewport = t._core && t._core._viewport;
        if (viewport && typeof viewport.queueSync === "function") viewport.queueSync();
      });
      fontSize = n;
    }
    // The room for the terminal in the box: its inner size less its padding.
    function room() {
      var cs = getComputedStyle(box);
      var px = function (v) { return parseFloat(v) || 0; };
      return {
        width: box.clientWidth - px(cs.paddingLeft) - px(cs.paddingRight),
        height: box.clientHeight - px(cs.paddingTop) - px(cs.paddingBottom),
      };
    }
    function screenSize() {
      var el = screenEl || mainHost.querySelector(".xterm-screen");
      return el ? { width: el.offsetWidth, height: el.offsetHeight } : null;
    }
    var laying = false;
    var layoutWidth = 0, layoutHeight = 0;
    function layout() {
      if (disposed || laying) return;
      laying = true;
      try {
        var r = room();
        var s = screenSize();
        // Not laid out yet (hidden, or not in the document): the next resize lays it out.
        if (!s || r.width <= 0 || r.height <= 0 || !s.width) { tellFit(); return; }
        if (fitOn) {
          var cellHeight = s.height / term.rows / fontSize;
          var want = fitFont({
            cols: term.cols, rows: term.rows, width: r.width, height: r.height,
            cellWidth: s.width / term.cols / fontSize, cellHeight: cellHeight,
          });
          // The height counts only when it leaves FONT_READABLE, as in fitFont.
          var tall = r.height / (term.rows * cellHeight) >= FONT_READABLE;
          setFont(want);
          // Cells round to device pixels: step down until the screen really fits (or the smallest font).
          for (var i = 0; i < 12 && fontSize > FONT_MIN; i++) {
            s = screenSize();
            if (s.width <= r.width && (!tall || s.height <= r.height)) break;
            setFont(Math.max(FONT_MIN, fontSize - 0.5));
          }
        }
        s = screenSize();
        // Narrower than the box (the font at its largest, or a size by hand): centred, with a quiet frame.
        if (r.width - s.width > 48) wrap.setAttribute("data-narrow", "");
        else wrap.removeAttribute("data-narrow");
        var cell = s.height / term.rows;
        var extra = cell > 0 ? Math.max(0, Math.min(MAX_HISTORY, Math.floor((r.height - s.height) / cell))) : 0;
        if (extra > 0) {
          if (hist.cols !== term.cols || hist.rows !== extra) {
            hist.resize(term.cols, extra);
            histPaintKey = "";
          }
          histHost.hidden = false;
        } else histHost.hidden = true;
        histDirty = true;
        schedulePaint();
        layoutWidth = box.clientWidth;
        layoutHeight = box.clientHeight;
        follow();
        moreRight();
      } finally {
        laying = false;
      }
      tellFit();
    }
    var layoutFrame = 0;
    function scheduleLayout() {
      if (layoutFrame || disposed) return;
      layoutFrame = requestAnimationFrame(function () { layoutFrame = 0; layout(); });
    }
    var observer = typeof ResizeObserver === "function" ? new ResizeObserver(scheduleLayout) : null;
    if (observer) observer.observe(box);
    else window.addEventListener("resize", scheduleLayout);
    // A web font that loads later changes the cell size.
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(scheduleLayout, function () {});
    // ---- The scrollback rows above the screen ----
    // The history terminal shows the screen buffer's lines just above what the screen shows (above the
    // viewer's scroll position too), copied cell by cell with their colours and styles. It is redrawn
    // when the screen scrolls, resizes or is reset, at most once a frame.
    var histDirty = true;
    var paintFrame = 0;
    var histKey = "";
    var histPaintKey = "";
    function schedulePaint() {
      if (paintFrame || disposed || histHost.hidden) return;
      paintFrame = requestAnimationFrame(function () { paintFrame = 0; paintHistory(); });
    }
    function paintHistory() {
      if (disposed || histHost.hidden) return;
      var b = term.buffer.active;
      // A full-screen program (the alternate screen) has no scrollback: the rows above it show, dimmed, the
      // normal screen it covers, up to its cursor line (what was there when the program started).
      var alt = b.type === "alternate";
      var src = alt ? term.buffer.normal : b;
      var top = alt ? Math.min(src.length, src.baseY + src.cursorY + 1) : b.viewportY;
      var key = b.type + ":" + top + ":" + src.baseY + ":" + src.length + ":" + hist.rows + ":" + hist.cols;
      if (!histDirty && key === histKey) return;
      histDirty = false;
      histKey = key;
      var out = [];
      var blanks = [];
      var cell = src.getNullCell();
      for (var y = top - hist.rows; y < top; y++) {
        var line = y >= 0 ? src.getLine(y) : null;
        var cols = term.cols;
        var next = y + 1 < top ? src.getLine(y + 1) : null;
        if (line && next && next.isWrapped) {
          line.getCell(cols - 1, cell);
          var emptyEnd = cell.getChars() === "" && cell.getWidth() === 1;
          next.getCell(0, cell);
          // A wide glyph can leave a null cell at the previous row's end.
          // Let xterm recreate it; writing a space would add one to copied text.
          if (emptyEnd && cell.getWidth() === 2) cols--;
        }
        // Let a full row wrap naturally when the source did, so copying a command
        // or URL does not insert a newline. Explicit LF resets stale wrap flags.
        if (out.length && !(line && line.isWrapped)) out.push("\r\n");
        out.push((line ? lineVt(line, cols, cell, alt, blanks, y - top + hist.rows + 1) : "") + "\x1b[0m\x1b[K");
      }
      // Home, each line then erase to its end (default colours); no newline after the last, so nothing scrolls.
      var vt = "\x1b[H\x1b[0m" + out.join("");
      // Full rows establish wrapping. Restore their unused cells afterwards: literal
      // padding spaces would change copied text (notably a shell continuation's backslash).
      blanks.forEach(function (run) {
        vt += "\x1b[" + run.row + ";" + (run.col + 1) + "H\x1b[" + run.sgr + "m\x1b[" + run.n + "X";
      });
      vt += "\x1b[0m";
      var painted = hist.cols + ":" + hist.rows + ":" + vt;
      if (painted === histPaintKey) return;
      histPaintKey = painted;
      // Never let an old selection silently refer to newly painted commands.
      // Identical history (e.g. output confined to the live prompt) keeps its selection.
      hist.clearSelection();
      hist.write(vt, function () { if (!disposed) hist.clearSelection(); });
    }
    // The history follows the screen's scroll: the wheel or a drag over it scrolls the screen.
    histHost.addEventListener("wheel", function (e) {
      // Keep browser pinch/zoom native; xterm's inner scrollbar must not consume it either.
      if (e.ctrlKey) { e.stopPropagation(); return; }
      if (!e.deltaY || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
      e.preventDefault();
      var cell = (screenSize() || { height: 0 }).height / term.rows || 16;
      var lines = e.deltaMode === 1 ? e.deltaY : e.deltaMode === 2 ? e.deltaY * hist.rows : e.deltaY / cell;
      e.stopPropagation();
      term.scrollLines(lines > 0 ? Math.max(1, Math.round(lines)) : Math.min(-1, Math.round(lines)));
    }, { passive: false, capture: true });
    var dragY = null;
    var dragRest = 0;
    histHost.addEventListener("touchstart", function (e) {
      dragY = e.touches.length === 1 ? e.touches[0].clientY : null;
      dragRest = 0;
    }, { passive: true, capture: true });
    histHost.addEventListener("touchmove", function (e) {
      if (dragY === null || e.touches.length !== 1) { dragY = null; return; }
      var cell = (screenSize() || { height: 0 }).height / term.rows || 16;
      dragRest += dragY - e.touches[0].clientY;
      dragY = e.touches[0].clientY;
      var lines = Math.trunc(dragRest / cell);
      if (lines) { dragRest -= lines * cell; term.scrollLines(lines); }
    }, { passive: true, capture: true });
    histHost.addEventListener("touchend", function () { dragY = null; }, { passive: true, capture: true });
    function jumpToLatest() { following = true; follow(); showJump(false); }
    jump.addEventListener("click", jumpToLatest);
    // No viewer answered: the CodeSense viewer on a Mac. The note says where it works.
    ready().catch(function () { if (current.state === "connecting") setState("error", "no_shell"); });
    api.dispose = function () {
      disposed = true;
      // Disposed while in control: the page hears control end here (the viewer ends it with the watch).
      if (ctlOn) setControl(false, "unwatched");
      dropPending();
      if (retryTimer) clearTimeout(retryTimer);
      if (terms[handle] === self) delete terms[handle];
      viewportCbs = viewportCbs.filter(function (cb) { return cb !== viewportGap; });
      if (window.visualViewport && keys) {
        window.visualViewport.removeEventListener("resize", viewportGap);
        window.visualViewport.removeEventListener("scroll", viewportGap);
      }
      if (observer) observer.disconnect(); else window.removeEventListener("resize", scheduleLayout);
      if (layoutFrame) cancelAnimationFrame(layoutFrame);
      if (paintFrame) cancelAnimationFrame(paintFrame);
      document.removeEventListener("copy", copyHistory, true);
      document.removeEventListener("pointerdown", clearHistorySelection, true);
      w.stop(); term.dispose(); hist.dispose(); wrap.remove();
      stateCbs = []; jumpCbs = []; resizeCbs = []; controlTermCbs = []; fitCbs = [];
    };
    // A size chosen by hand turns fit off; setFit(true) turns it on again.
    api.setFontSize = function (n) { fitOn = false; setFont(clampFont(n)); layout(); return fontSize; };
    api.fontSize = function () { return fontSize; };
    api.setFit = function (on) { fitOn = on !== false; layout(); return fitOn; };
    api.fitting = function () { return fitOn; };
    api.jumpToLatest = jumpToLatest;
    api.size = function () { return { cols: term.cols, rows: term.rows }; };
    return api;
  }
  // Half pixels: the fitted sizes are, and a page may set one.
  function clampFont(n) { return Math.max(FONT_MIN, Math.min(FONT_MAX, Math.round((Number(n) || FONT_DEFAULT) * 2) / 2)); }
  // The largest font, in half pixels within FONT_MIN and FONT_MAX, at which `cols` cells fill no more than
  // `width`, and `rows` cells no more than `height` when that leaves FONT_READABLE or more; when it does not
  // (a phone on its side) the width alone, and the screen scrolls, newest line at the bottom. cellWidth and
  // cellHeight are a cell's size per px of font.
  function fitFont(o) {
    var cols = Math.max(1, Number(o && o.cols) || 80);
    var rows = Math.max(1, Number(o && o.rows) || 24);
    var cw = Number(o && o.cellWidth) || 0.6;
    var ch = Number(o && o.cellHeight) || 1.2;
    var byWidth = (Number(o && o.width) || 0) / (cols * cw);
    var byHeight = o && o.height > 0 ? o.height / (rows * ch) : Infinity;
    var f = Math.floor((byHeight >= FONT_READABLE ? Math.min(byWidth, byHeight) : byWidth) * 2) / 2;
    return Math.max(FONT_MIN, Math.min(FONT_MAX, isFinite(f) ? f : FONT_DEFAULT));
  }
  // One buffer line as VT: its characters with SGR for each change of colour or style.
  function lineVt(line, cols, cell, dim, blanks, row) {
    var out = "";
    var last = "0";
    var n = Math.min(cols, line.length);
    for (var x = 0; x < n; x++) {
      line.getCell(x, cell);
      if (cell.getWidth() === 0) continue;
      var sgr = sgrOf(cell, dim);
      if (sgr !== last) { out += "\x1b[" + sgr + "m"; last = sgr; }
      var chars = cell.getChars();
      if (!chars) {
        var run = blanks[blanks.length - 1];
        if (run && run.row === row && run.col + run.n === x && run.sgr === sgr) run.n++;
        else blanks.push({ row: row, col: x, n: 1, sgr: sgr });
      }
      out += chars || " ";
    }
    return out;
  }
  function sgrOf(c, dim) {
    var p = ["0"];
    if (c.isBold()) p.push("1");
    if (dim || c.isDim()) p.push("2");
    if (c.isItalic()) p.push("3");
    if (c.isUnderline()) p.push("4");
    if (c.isBlink()) p.push("5");
    if (c.isInverse()) p.push("7");
    if (c.isInvisible()) p.push("8");
    if (c.isStrikethrough()) p.push("9");
    if (c.isOverline && c.isOverline()) p.push("53");
    colour(p, c.isFgRGB(), c.isFgPalette(), c.getFgColor(), 30);
    colour(p, c.isBgRGB(), c.isBgPalette(), c.getBgColor(), 40);
    return p.join(";");
  }
  function colour(p, rgb, palette, v, base) {
    if (rgb) p.push(base + 8, 2, (v >> 16) & 255, (v >> 8) & 255, v & 255);
    else if (palette) {
      if (v < 8) p.push(base + v);
      else if (v < 16) p.push(base + 60 + v - 8);
      else p.push(base + 8, 5, v);
    }
  }
  // Bracketed-paste markers removed until none is left (a paste of only markers types nothing).
  // One linear pass: a marker that ends what is kept so far is dropped (the same result as removing until none is left).
  function stripMarkersAll(text) {
    var out = [];
    for (var i = 0; i < text.length; i++) {
      out.push(text[i]);
      var n = out.length;
      if (text[i] === "~" && n >= 6 && out[n - 6] === "\x1b" && out[n - 5] === "[" && out[n - 4] === "2" && out[n - 3] === "0" && (out[n - 2] === "0" || out[n - 2] === "1")) out.length = n - 6;
    }
    return out.join("");
  }
  // K38: the largest keys or paste message, in UTF-8 bytes.
  var MAX_INPUT = 16 * 1024;
  function utf8Length(text) {
    var n = 0;
    for (var i = 0; i < text.length; i++) {
      var c = text.charCodeAt(i);
      if (c < 0x80) n += 1;
      else if (c < 0x800) n += 2;
      else if (c >= 0xd800 && c <= 0xdbff && i + 1 < text.length && (text.charCodeAt(i + 1) & 0xfc00) === 0xdc00) { n += 4; i++; }
      else n += 3;
    }
    return n;
  }
  // Pieces of at most `max` UTF-8 bytes, never splitting a character.
  function chunks(text, max) {
    var out = [];
    var piece = "";
    var size = 0;
    for (var ch of text) {
      var c = ch.codePointAt(0);
      var n = c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
      if (size + n > max) {
        // Cut after the last line break in the piece's final 2 KB, if there is one (not between a CR and its LF).
        var cut = -1;
        for (var j = piece.length - 1; j >= 0 && piece.length - j <= 2048; j--) {
          var at = piece.charAt(j);
          var after = j + 1 < piece.length ? piece.charAt(j + 1) : ch;
          if (at === "\n" || (at === "\r" && after !== "\n")) { cut = j; break; }
        }
        if (cut >= 0) {
          out.push(piece.slice(0, cut + 1));
          piece = piece.slice(cut + 1);
          size = utf8Length(piece);
        } else { out.push(piece); piece = ""; size = 0; }
      }
      piece += ch;
      size += n;
    }
    if (piece) out.push(piece);
    return out;
  }
  // A terminal's answer to a query (DA, DSR/CPR, focus in/out, OSC/DCS replies), not a key.
  var REPORTS = [/^\x1b\[[?>=]?[\d;]*[cn]$/, /^\x1b\[[IO]$/, /^\x1b\][\s\S]*(\x07|\x1b\\)$/, /^\x1bP[\s\S]*\x1b\\$/, /^\x1b\[\?[\d;]*\$y$/];
  function isReport(data) { return REPORTS.some(function (r) { return r.test(data); }); }

  // Input to a terminal the owner controls (P15 input). The viewer checks again
  // and sends nothing for a terminal not under control; the relay and the Mac check once more.
  function controlled(handle) {
    var t = terms[handle];
    var wid = t && t.on() ? t.wid() : null;
    return wid;
  }
  function sendInput(handle, text, o) {
    var wid = controlled(handle);
    if (wid === null) return Promise.reject(new Error("not_controlling"));
    if (typeof text !== "string" || text === "") return Promise.reject(new Error("bad_request"));
    if (utf8Length(text) > MAX_INPUT) return Promise.reject(new Error("too_large"));
    if (o && o.paste && !stripMarkersAll(text)) return Promise.reject(new Error("bad_request"));
    // Through the terminal's own queue, so it never lands inside a paste still being sent.
    var sent = terms[handle].send(text, o && o.paste ? "paste" : "keys");
    if (sent !== true) return Promise.reject(new Error(sent === "too_large" ? "too_large" : "not_controlling"));
    return Promise.resolve();
  }
  function resize(handle, cols, rows) {
    var wid = controlled(handle);
    if (wid === null) return Promise.reject(new Error("not_controlling"));
    if (!Number.isInteger(cols) || !Number.isInteger(rows) || cols < 1 || cols > MAX_COLS || rows < 1 || rows > MAX_ROWS) return Promise.reject(new Error("bad_request"));
    var done = terms[handle].resize(cols, rows);
    if (done !== true) return Promise.reject(new Error(done === "too_large" ? "too_large" : "not_controlling"));
    return Promise.resolve();
  }
  function onControl(cb) {
    controlCbs.push(cb);
    return function () { controlCbs = controlCbs.filter(function (c) { return c !== cb; }); };
  }

  /*@kit-core-start*/
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
  /*@kit-core-end*/
  csCore.register("hosted", {
    present: function () { return true; },
    ready: ready,
    // A paging read (queryAll) keeps its own rate_limited backoff, so it skips the relay retry.
    ask: function (name, args, o) {
      var signal = o && o.signal;
      function wire() { return o && o.paging ? wireQuery(name, args, signal) : wirePublic(name, args, signal); }
      // Before the viewer has said hello a read waits for it (rejecting no_shell after 3 s); once it has, the ask goes out at once.
      return hello ? wire() : ready().then(wire);
    },
    storyTotals: storyTotals,
    subscribe: subscribe,
  });

  return Object.assign({
    version: VERSION,
    ready: ready,
    role: function () { return hello ? hello.role : null; },
    query: publicQuery,
    queryAll: queryAll,
    storyTotals: storyTotals,
    act: act,
    onActions: listOf("actions", "actions"),
    subscribe: subscribe,
    subscribeAll: subscribeAll,
    partialList: partialList,
    onPresence: onPresence,
    viewer: viewer,
    endOwnerAccess: endOwnerAccess,
    watch: watch,
    listAgents: listOf("terminals", "terminals"),
    inbox: listOf("inbox", "messages"),
    send: sendMessage,
    react: react,
    markRead: markRead,
    confirmHint: confirmHint,
    media: media,
    listWorkspaces: listOf("workspaces", "workspaces"),
    watchTerminal: watchTerminal,
    sendInput: sendInput,
    resize: resize,
    onControl: onControl,
    fontRange: { min: FONT_MIN, max: FONT_MAX, initial: FONT_DEFAULT },
    fitFont: fitFont,
    passing: function (reason) { return PASSING[reason] === true; },
  }, csCore.api);
})();
