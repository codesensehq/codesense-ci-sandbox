// Written by create_dashboard (template: workspaces, SW-DP-21). The open
// workspaces of this dashboard's team on the owner's Mac, their agent sessions,
// and a read-only live terminal for the one tapped. It uses cs-live.js (the
// CodeSense dashboard toolkit), which talks to the viewer only by postMessage.
// dashboard.yml declares the three streams: live.streams [workspaces, terminals, terminal].
(function () {
  "use strict";
  function $(id) { return document.getElementById(id); }
  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined) n.textContent = text;
    return n;
  }
  var STATES = { running: "Running", "needs-you": "Needs you", idle: "Idle", exited: "Exited", offline: "Offline", unknown: "Unknown" };
  var ORDER = { "needs-you": 0, running: 1, idle: 2, unknown: 3, offline: 4, exited: 5 };
  var CHEVRON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 5l7 7-7 7"/></svg>';
  var desktop = window.matchMedia("(min-width: 900px)");

  var triage = document.body.getAttribute("data-template") === "bug-triage";
  var bugs = null;
  var activity = [];
  var pendingActions = Object.create(null);
  var actionNotes = Object.create(null);
  var bugError = null;
  var activityStale = false;
  var mission = document.body.getAttribute("data-template") === "mission-control";
  var inboxStop = null, inboxRows = [], inboxState = "connecting", replyId = null, sending = false;
  var drafts = new Map();
  function draftKey() { return replyId === null ? "new" : "reply:" + replyId; }
  function saveDraft() {
    if (!$("compose").hidden) drafts.set(draftKey(), {text: $("message-text").value, to: $("recipient").value});
  }
  function inboxPermissions() {
    if (!mission) return;
    var owner = CS.role() === "owner";
    $("new-message").hidden = !owner;
    $("inbox-permission").hidden = owner;
    if (!owner) $("compose").hidden = true;
    $("send-message").disabled = sending || inboxState !== "live";
  }
  function composer(row) {
    if (CS.role() !== "owner" || sending) return;
    saveDraft();
    replyId = row ? row.messageId : null;
    var draft = drafts.get(draftKey());
    $("compose-heading").textContent = row ? "Reply to " + row.fromName : "New message";
    $("recipient").value = row ? row.from : draft ? draft.to : "human";
    $("message-text").value = draft ? draft.text : "";
    if (!row) { $("thread").hidden = true; delete $("thread").dataset.message; }
    $("recipient").readOnly = !!row;
    $("recipient-help").textContent = row ? "Replies follow the sender's reply address within this team." : "Choose a session, use human for your inbox, or role:triage for a team role.";
    $("send-status").textContent = "";
    $("compose").hidden = false;
    $("inbox-panel").classList.add("inbox-reading"); $("inbox-back").hidden = false;
    inboxPermissions();
    $("message-text").focus();
  }
  function showThread(row, refresh) {
    if (!refresh) { saveDraft(); $("compose").hidden = true; }
    var thread = $("thread"); thread.replaceChildren(); thread.hidden = false;
    thread.dataset.message = String(row.messageId);
    $("messages").querySelectorAll(".message-row").forEach(function (button) {
      button.setAttribute("aria-current", String(button.dataset.message === thread.dataset.message));
    });
    if (!refresh) {
      $("inbox-panel").classList.add("inbox-reading"); $("inbox-back").hidden = false;
      $("inbox-back").focus();
    }
    var related = new Set([row.messageId]), changed = true;
    while (changed) {
      changed = false;
      inboxRows.forEach(function (m) {
        if (related.has(m.messageId) && m.inReplyTo && !related.has(m.inReplyTo)) { related.add(m.inReplyTo); changed = true; }
        if (related.has(m.inReplyTo) && !related.has(m.messageId)) { related.add(m.messageId); changed = true; }
      });
    }
    inboxRows.filter(function (m) { return related.has(m.messageId); }).slice().reverse().forEach(function (m) {
      var article = el("article", "mail");
      article.appendChild(el("h3", "", m.fromName || m.from));
      article.appendChild(el("p", "inbox-meta", new Date(m.createdAt).toLocaleString()));
      article.appendChild(el("p", "mail-body", m.text === undefined ? (m.summary || "Message body is not shared by this dashboard.") : m.text));
      thread.appendChild(article);
    });
    if (CS.role() === "owner") {
      var reply = el("button", "", "Reply"); reply.type = "button";
      reply.dataset.reply = String(row.messageId);
      reply.addEventListener("click", function () { composer(row); }); thread.appendChild(reply);
    }
  }
  function renderInbox(rows, state, detail) {
    var focused = document.activeElement, focusMessage = focused && focused.dataset.message, focusReply = focused && focused.dataset.reply;
    inboxRows = rows; inboxState = state;
    $("messages").setAttribute("aria-busy", String(state === "connecting"));
    $("inbox-status").textContent = state === "live" ? rows.length + (rows.length === 1 ? " recent message" : " recent messages")
      : state === "connecting" ? "Loading messages…"
      : state === "paused" ? "Inbox paused. Messages will update when the connection returns."
      : "Inbox unavailable" + (detail === "not_declared" ? ". This dashboard no longer shares its inbox." : ". Reopen Inbox to try again.");
    var list = $("messages"); list.replaceChildren();
    if (!rows.length && state === "live") {
      list.appendChild(el("h3", "", "No messages yet"));
      list.appendChild(el("p", "inbox-meta", "Messages for this team will appear here."));
    }
    rows.forEach(function (row) {
      var button = el("button", "message-row"); button.type = "button";
      button.dataset.message = String(row.messageId);
      button.appendChild(el("strong", "", row.fromName || row.from));
      button.appendChild(el("span", "message-summary", row.summary || (row.text ? row.text.slice(0, 120) : "Message body not shared")));
      button.appendChild(el("span", "inbox-meta", (row.unread ? "Unread · " : "") + new Date(row.createdAt).toLocaleString() + (row.delivery ? " · " + row.delivery.state : "")));
      button.addEventListener("click", function () { showThread(row); $("thread").scrollIntoView({block: "nearest"}); });
      list.appendChild(button);
    });
    var selectedMessage = rows.find(function (r) { return String(r.messageId) === $("thread").dataset.message; });
    if (selectedMessage) showThread(selectedMessage, true);
    var restore = focusMessage ? list.querySelector('[data-message="' + focusMessage + '"]')
      : focusReply ? $("thread").querySelector('[data-reply="' + focusReply + '"]') : null;
    if (restore) restore.focus({preventScroll: true});
    inboxPermissions();
  }
  function showInbox(on) {
    if (on && view) close();
    $("split").hidden = on; $("inbox-panel").hidden = !on;
    $("agents-view").setAttribute("aria-pressed", String(!on)); $("inbox-view").setAttribute("aria-pressed", String(on));
    if (inboxStop) { inboxStop(); inboxStop = null; }
    if (on) {
      renderInbox(inboxRows, "connecting");
      inboxStop = CS.inbox(renderInbox);
      $("recipients").replaceChildren();
      ["human"].concat(agents.map(function (a) { return a.handle; })).forEach(function (h) { var o = el("option"); o.value = h; $("recipients").appendChild(o); });
    }
  }
  if (mission) {
    $("views").hidden = false;
    $("agents-view").addEventListener("click", function () { showInbox(false); });
    $("inbox-view").addEventListener("click", function () { showInbox(true); });
    $("new-message").addEventListener("click", function () { composer(null); });
    $("inbox-back").addEventListener("click", function () {
      $("inbox-panel").classList.remove("inbox-reading");
      $("inbox-back").hidden = true;
      var first = $("messages").querySelector("button"); if (first) first.focus();
    });
    $("cancel-message").addEventListener("click", function () {
      if (sending) return;
      saveDraft(); $("compose").hidden = true;
      if ($("thread").hidden) { $("inbox-panel").classList.remove("inbox-reading"); $("inbox-back").hidden = true; }
      $("new-message").focus();
    });
    $("compose").addEventListener("submit", function (event) { event.preventDefault(); });
    // The hosted sandbox forbids native form submission; send over the page API.
    $("send-message").addEventListener("click", function (event) {
      event.preventDefault();
      if (sending || inboxState !== "live" || CS.role() !== "owner") return;
      var text = $("message-text").value;
      if (!text.trim() || Array.from(text).length > 2000) { $("send-status").textContent = "Enter between 1 and 2,000 characters."; return; }
      sending = true; inboxPermissions(); $("send-status").textContent = "Sending…";
      $("message-text").readOnly = true; $("recipient").readOnly = true;
      CS.send($("recipient").value, text, replyId === null ? undefined : replyId).then(function () {
        drafts.delete(draftKey());
        $("message-text").value = ""; $("send-status").textContent = "Message sent.";
      }).catch(function (error) {
        var errors = {not_allowed: "Only the owner can send. Sign in again to continue.", not_found: "That recipient or message is no longer available in this team.", rate_limited: "You have reached the message limit. Try again later.", invalid_args: "Check the recipient and use 1 to 2,000 characters.", mac_offline: "Your Mac is offline. Reconnect before sending."};
        $("send-status").textContent = errors[error.message] || "Delivery was not confirmed. Check the inbox before sending again.";
      }).finally(function () { sending = false; $("message-text").readOnly = false; $("recipient").readOnly = replyId !== null; inboxPermissions(); });
    });
  }
  if (mission || triage) {
    $("list").setAttribute("aria-label", triage ? "Bugs and activity" : "Agents");
    $("back").setAttribute("aria-label", triage ? "Back to bugs" : "Back to agents");

    $("pane-empty").textContent = "Pick an agent to watch its terminal.";
    $("role").hidden = false;
    $("role").textContent = "Checking your role";
  }
  var ownerControl = false;
  function renderRole() {
    if ((!mission && !triage) || CS.role() === null) return;
    if (triage) { $("role").textContent = CS.role() === "owner" ? "Owner · Choose a bug to work on" : "Viewer · Follow progress and watch agents"; return; }
    inboxPermissions();
    var text = CS.role() === "owner"
      ? (view && view.controlling() ? "Owner · Controlling " + $("who-name").textContent
        : ownerControl ? "Owner · Choose a terminal, then Take control in the top bar"
        : "Owner · Control is off for this dashboard")
      : "Viewer · View only";
    if ($("role").textContent !== text) $("role").textContent = text;
  }

  var agents = [];
  var workspaces = [];
  var agentsState = "connecting";
  var listsLive = false;
  var updatedAt = null;
  var mac = null;
  var selected = null;
  var view = null;
  var fontSize = CS.fontRange.initial;
  // The terminal's font fits its columns to the pane's width until a size is chosen by hand.
  var fit = true;
  var returnFocus = null;
  var ended = null;
  // The workspaces stream ended for a passing reason: its headings are the last ones.
  var workspacesStale = false;
  // Why the list stream ended (no_team, not_found, not_declared, access_ended, failed...), or null.
  var listEnd = null;
  var LIST_ENDS = {
    no_team: ["This dashboard has no team.", "Its folder is not in a team's context, or CodeSense did not know the team when it was published. Move the folder into a team's content folder and publish it again."],
    not_found: ["This dashboard moved to another team.", "Publish it again from its new place to list that team's agents."],
    not_declared: ["This dashboard no longer lists agents.", "Its live block does not declare the terminals stream any more."],
    access_ended: ["Your access to this dashboard has ended.", "Ask the owner for a new invite."],
    replaced: ["This dashboard is open in another window.", "Reload this page to watch here instead."],
  };

  function stateOf(row) { return STATES[row.state] ? row.state : "unknown"; }
  function sortRows(rows) {
    return rows.slice().sort(function (a, b) {
      return (ORDER[stateOf(a)] - ORDER[stateOf(b)]) || String(a.displayName).localeCompare(String(b.displayName));
    });
  }
  function subline(row) {
    // The story is already in the workspace heading when it is that workspace's story.
    var story = row.story && (mission || !(row.workspace && row.workspace.story === row.story)) ? row.story : null;
    if (mission) return [row.workspace && wsName(row.workspace), story, row.role, row.agent && String(row.agent).replace(/^builtin\//, "")].filter(Boolean).join(" · ") || "Agent session";
    return [row.role, row.agent && String(row.agent).replace(/^builtin\//, ""), story].filter(Boolean).join(" · ") || "Agent session";
  }
  // A workspace's heading: its name, or for an id that still carries a folder (a Mac app older than
  // the friendly names), the folder's own name, never the whole path.
  function wsName(w) {
    var n = (w && (w.name || w.id)) || "";
    var m = /^(?:generic|root|story):(.*)$/.exec(n);
    if (!m || !/[\/\\]/.test(m[1])) return n;
    var parts = m[1].split(/[\/\\]/).filter(Boolean);
    return parts.length ? parts[parts.length - 1] : "Workspace";
  }
  function clock(at) { return at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }); }

  function renderStatus() {
    var s = $("status");
    s.textContent = "";
    var asks = agents.filter(function (r) { return stateOf(r) === "needs-you"; }).length;
    if (agentsState === "no_shell") { s.textContent = "Not connected"; return; }
    if (listEnd) { s.textContent = CS.passing(listEnd) ? "Reconnecting" : "Not listing agents"; return; }
    if (agentsState === "paused" || agentsState === "ended" || agentsState === "error" || mac === "offline") {
      // The banner above the list says since when.
      s.textContent = mac === "offline" ? "Owner's Mac is offline" : "Reconnecting";
      return;
    }
    if (!listsLive) { s.textContent = "Connecting to the owner's Mac"; return; }
    s.textContent = "Mac online · " + agents.length + (agents.length === 1 ? " agent" : " agents");
    if (asks > 0) {
      s.appendChild(document.createTextNode(" · "));
      s.appendChild(el("span", "ask", asks + " need" + (asks === 1 ? "s" : "") + " you"));
    }
  }

  function rowButton(row) {
    var b = el("button", "row");
    b.type = "button";
    b.setAttribute("data-handle", row.handle);
    if (selected === row.handle) b.setAttribute("aria-current", "true");
    var st = stateOf(row);
    b.appendChild(el("span", "dot " + st));
    b.appendChild(el("span", "row-name", row.displayName || row.handle));
    b.appendChild(el("span", "row-state" + (st === "needs-you" ? " ask" : ""), STATES[st]));
    b.insertAdjacentHTML("beforeend", CHEVRON);
    b.appendChild(el("span", "row-sub", subline(row)));
    b.setAttribute("aria-label", (row.displayName || row.handle) + ", " + STATES[st] + (mission ? ", " + subline(row) : "") + ". Watch terminal");
    // Focus moves to Back only for a keyboard user (a click with detail 0); a tap keeps no ring.
    b.addEventListener("click", function (e) { open(row.handle, b, e.detail === 0); });
    return b;
  }

  function section(title, meta, rows, tabs) {
    var s = el("section", "ws");
    var head = el("div", "ws-head");
    head.appendChild(el("h2", "", title));
    var asks = rows.filter(function (r) { return stateOf(r) === "needs-you"; }).length;
    if (asks) head.appendChild(el("span", "ws-count", asks + " need" + (asks === 1 ? "s" : "") + " you"));
    s.appendChild(head);
    if (meta) s.appendChild(el("p", "ws-meta", meta));
    if (!rows.length) {
      s.appendChild(el("p", "ws-none", tabs ? "No agents to watch here (" + tabs + (tabs === 1 ? " tab" : " tabs") + " open)." : "No agents to watch here."));
      return s;
    }
    var ul = el("ul", "rows");
    sortRows(rows).forEach(function (r) { var li = el("li"); li.appendChild(rowButton(r)); ul.appendChild(li); });
    s.appendChild(ul);
    return s;
  }

  function renderList() {
    if (triage) return renderTriage();
    var list = $("list");
    renderStatus();
    if (agentsState === "no_shell") {
      list.setAttribute("aria-busy", "false");
      list.replaceChildren(empty("Live terminals work in the published dashboard.", "Open this dashboard from its codesense.ai link to see the owner's agents and watch their terminals live."));
      return;
    }
    // A passing end keeps the last list under a banner while the watch waits to retry.
    if (listEnd && !(CS.passing(listEnd) && updatedAt)) {
      // The list stream is over: say why instead of a list that never updates.
      var why = LIST_ENDS[listEnd] || (CS.passing(listEnd)
        ? ["The owner's Mac could not list its agents.", "Trying again shortly."]
        : ["The agent list stopped.", "The owner's Mac could not list its agents (" + listEnd + "). Reload the page to try again."]);
      list.setAttribute("aria-busy", "false");
      list.replaceChildren(empty(why[0], why[1]));
      return;
    }
    if (!listsLive && !updatedAt) return;
    list.setAttribute("aria-busy", "false");
    var stale = agentsState !== "live" || mac === "offline" || workspacesStale;
    var retrying = listEnd && CS.passing(listEnd);
    var byHandle = Object.create(null);
    agents.forEach(function (r) { byHandle[r.handle] = r; });
    var placed = Object.create(null);
    var out = [];
    (mission ? [] : workspaces).forEach(function (w) {
      var rows = (w.sessions || []).map(function (h) { placed[h] = true; return byHandle[h]; }).filter(Boolean);
      var meta = [w.story && w.story !== w.name ? w.story : null, w.branch].filter(Boolean).join(" · ");
      out.push(section(wsName(w), meta, rows, w.tabs || 0));
    });
    var rest = agents.filter(function (r) { return !placed[r.handle]; });
    if (rest.length) out.push(section(mission ? "Agents" : workspaces.length ? "Other sessions" : "Sessions", "", rest, 0));
    if (!out.length) {
      out.push(empty("No agents are running in this team right now.", "Sessions the owner starts in this team's workspaces on their Mac appear here within a few seconds. Tap one to watch its terminal."));
    }
    if (stale && out.length) {
      var note = mac === "offline" ? "The owner's Mac is offline." : retrying ? "The owner's Mac could not list its agents; trying again shortly." : agentsState === "live" && workspacesStale ? "Workspace headings may be out of date; trying again shortly." : "Reconnecting to the owner's Mac.";
      if (updatedAt) note += " Showing the list from " + clock(updatedAt) + ".";
      out.unshift(el("p", "banner", note));
    }
    var focusHandle = document.activeElement && document.activeElement.getAttribute && document.activeElement.getAttribute("data-handle");
    list.replaceChildren.apply(list, out);
    if (focusHandle) { var again = list.querySelector('[data-handle="' + CSS.escape(focusHandle) + '"]'); if (again) again.focus(); }
    if (view) renderWho();
    renderRole();
  }

  function empty(title, text) {
    var d = el("div", "empty");
    d.appendChild(el("h2", "", title));
    d.appendChild(el("p", "", text));
    return d;
  }

  function renderTriage() {
    var list = $("list");
    renderStatus();
    list.setAttribute("aria-busy", bugs === null && !bugError ? "true" : "false");
    if (bugs === null && !bugError) return;
    var focus = document.activeElement && document.activeElement.dataset && document.activeElement.dataset.actionKey;
    var out = [];
    if (mac === "offline") out.push(el("p", "banner", "The owner's Mac is offline. Actions are unavailable until it reconnects."));
    if (bugError) out.push(el("p", "banner", bugError === "no_shell" ? "Publish this dashboard to load bugs and run actions." : "Bugs could not be loaded. Waiting for the next update."));
    if (bugs && !bugs.length && !bugError) out.push(empty("No bugs to triage", "Bugs in this team's context appear here when their type is bug."));
    (bugs || []).forEach(function (bug) {
      var item = el("article", "bug");
      item.appendChild(el("h2", "bug-title", bug.title || bug.fields && bug.fields.title || bug.id));
      item.appendChild(el("p", "bug-meta", bug.id + " · " + (bug.fields && bug.fields.status || "No status")));
      var bar = el("div", "bug-actions");
      var buttons = [["needsInfo", "Needs info"], ["wontFix", "Won't fix"]];
      if (document.body.dataset.workIt === "true") buttons.unshift(["workIt", "Work it"]);
      buttons.forEach(function (pair) {
        var b = el("button", "bug-action", pendingActions[bug.id] === pair[0] ? "Working…" : pair[1]);
        b.type = "button"; b.dataset.actionKey = bug.id + ":" + pair[0];
        b.setAttribute("aria-disabled", CS.role() !== "owner" || mac !== "online" || !!pendingActions[bug.id] ? "true" : "false");
        b.addEventListener("click", function () {
          if (CS.role() !== "owner" || mac !== "online" || pendingActions[bug.id]) return;
          pendingActions[bug.id] = pair[0]; actionNotes[bug.id] = "Sending " + pair[1].toLowerCase() + "…"; renderTriage();
          CS.act(pair[0], {id: bug.id}).then(function (result) {
            actionNotes[bug.id] = pair[1] + " completed.";
            if (result.fields && bug.fields) Object.assign(bug.fields, result.fields);
          }).catch(function (e) {
            var errors = {rate_limited: "Action limit reached. Try again later.", busy: "The dashboard's agents are busy. Wait for one to finish.", not_allowed: "Only the owner can run actions.", not_found: "This bug or its configured agent is no longer available.", timeout: "Outcome unknown. Check activity before trying again.", mac_offline: "The owner's Mac is offline. Try again when it reconnects."};
            actionNotes[bug.id] = errors[e.message] || "Action refused. Ask the owner to check its declaration.";
          }).finally(function () { delete pendingActions[bug.id]; renderTriage(); });
        });
        bar.appendChild(b);
      });
      item.appendChild(bar);
      var note = el("p", "action-note", actionNotes[bug.id] || ""); note.setAttribute("role", "status"); item.appendChild(note);
      out.push(item);
    });
    var feed = el("section", "activity"); feed.appendChild(el("h2", "", "Activity"));
    if (activityStale) feed.appendChild(el("p", "bug-meta", "Activity is reconnecting. Showing the last update."));
    if (!activity.length) feed.appendChild(el("p", "bug-meta", "Actions and agent progress will appear here."));
    activity.slice().reverse().forEach(function (r) {
      var row = el("div", "activity-row");
      var names = {workIt: "Work it", needsInfo: "Needs info", wontFix: "Won't fix"};
      row.appendChild(el("p", "", (names[r.action] || r.action) + (r.id ? " · " + r.id : "")));
      row.appendChild(el("p", "bug-meta", (r.by && r.by.role === "owner" ? "By owner" : "By a teammate") + " · " + (r.state || r.outcome) + " · " + new Date(r.at).toLocaleTimeString([], {hour: "2-digit", minute: "2-digit"})));
      if (r.handle) { var b = el("button", "bug-action", "Watch agent"); b.type = "button"; b.dataset.actionKey = "watch:" + r.handle + ":" + r.at; b.addEventListener("click", function (e) { open(r.handle, b, e.detail === 0); }); row.appendChild(b); }
      feed.appendChild(row);
    });
    out.push(feed); list.replaceChildren.apply(list, out);
    if (focus) { var target = list.querySelector('[data-action-key="' + CSS.escape(focus) + '"]'); if (target) target.focus(); }
  }

  function renderWho() {
    var row = agents.filter(function (r) { return r.handle === selected; })[0];
    if (!row) return;
    // An ended terminal shows its end, whatever the list said last.
    var st = ended === "exited" ? "exited" : stateOf(row);
    $("who-dot").className = "dot " + st;
    $("who-name").textContent = row.displayName || row.handle;
    var ws = row.workspace ? wsName(row.workspace) : null;
    var size = view ? view.size() : null;
    $("who-meta").textContent = [STATES[st], ws, size ? size.cols + "×" + size.rows : null].filter(Boolean).join(" · ");
  }

  function open(handle, from, keyboard) {
    if (view) view.dispose();
    selected = handle;
    returnFocus = from || null;
    document.body.classList.add("watching");
    $("pane-empty").hidden = true;
    $("viewer").hidden = false;
    Array.prototype.forEach.call(document.querySelectorAll(".row"), function (b) {
      if (b.getAttribute("data-handle") === handle) b.setAttribute("aria-current", "true"); else b.removeAttribute("aria-current");
    });
    ended = null;
    view = CS.watchTerminal(handle, $("term"), { fontSize: fontSize, fit: fit });
    // Use the toolkit's selected-theme tokens for the surrounding toolbar too.
    var terminalStyle = $("term").querySelector(".cs-term-wrap").style;
    Array.prototype.forEach.call(terminalStyle, function (name) {
      if (name.indexOf("--cs-term-") === 0) $("viewer").style.setProperty(name, terminalStyle.getPropertyValue(name));
    });
    view.onResize(renderWho);
    // The fitted size changes with the pane (a rotation, the key bar): the control shows it.
    view.onFit(function (e) { fontSize = e.fontSize; fit = e.fit; showFont(); });
    // "View only" is not true while the owner controls (the shell's bar says so): the line goes, and the terminal keeps its row.
    $("notice").hidden = false;
    view.onControl(function (e) {
      if (e.reason === "control_off" || e.error === "control_off") ownerControl = false;
      else if (e.on) ownerControl = true;
      $("notice").hidden = e.on;
      renderRole();
    });
    // The toolkit shows paused, ended and error states under the terminal itself.
    // The toolkit shows the terminal's states under it and opens it again after a passing end.
    view.onState(function (s, detail) {
      ended = s === "ended" ? detail : s === "live" ? null : ended;
      renderWho();
    });
    showFont();
    renderWho();
    renderRole();
    if (keyboard && !desktop.matches) $("back").focus();
  }

  function close() {
    if (view) { view.dispose(); view = null; }
    selected = null;
    renderRole();
    document.body.classList.remove("watching");
    $("viewer").hidden = true;
    $("pane-empty").hidden = false;
    Array.prototype.forEach.call(document.querySelectorAll(".row[aria-current]"), function (b) { b.removeAttribute("aria-current"); });
    if (returnFocus && document.body.contains(returnFocus)) returnFocus.focus();
  }

  function showFont() {
    $("font-size").textContent = String(fontSize);
    $("smaller").disabled = fontSize <= CS.fontRange.min;
    $("larger").disabled = fontSize >= CS.fontRange.max;
    $("fit").setAttribute("aria-pressed", fit ? "true" : "false");
  }
  // A step by hand goes to the next whole pixel (a fitted 7.5 goes to 8 or 7) and turns fit off.
  function font(step) {
    var next = step > 0 ? Math.floor(fontSize) + 1 : Math.ceil(fontSize) - 1;
    fontSize = Math.max(CS.fontRange.min, Math.min(CS.fontRange.max, next));
    fit = false;
    if (view) fontSize = view.setFontSize(fontSize);
    showFont();
  }
  $("smaller").addEventListener("click", function () { font(-1); });
  $("larger").addEventListener("click", function () { font(1); });
  $("fit").addEventListener("click", function () {
    fit = !fit;
    if (view) view.setFit(fit);
    showFont();
  });
  $("back").addEventListener("click", close);
  document.addEventListener("keydown", function (e) { if (e.key === "Escape" && view && !desktop.matches) close(); });

  // Each list is watched again a little after a passing end (the owner's Mac could
  // not serve it for a moment), on its own, waiting longer each time up to two minutes.
  var RETRY_MS = 15000;
  function retrying(start) {
    var stop = null;
    var timer = 0;
    var delay = RETRY_MS;
    function run() {
      timer = 0;
      // The ended watch is let go first, or it would open again on the next page show.
      if (stop) stop();
      stop = start(function () { delay = RETRY_MS; if (timer) { clearTimeout(timer); timer = 0; } }, function () {
        if (timer) return;
        timer = setTimeout(run, delay);
        delay = Math.min(delay * 2, 120000);
      });
    }
    run();
  }
  retrying(function (live, again) {
    return CS.listAgents(function (rows, state, detail) {
      agentsState = state;
      listEnd = state === "ended" || state === "error" ? detail || state : null;
      if (state === "live") { agents = rows; listsLive = true; updatedAt = new Date(); live(); }
      if (listEnd && CS.passing(listEnd)) again();
      renderList();
    });
  });
  if (!mission && !triage) retrying(function (live, again) {
    return CS.listWorkspaces(function (list, state, detail) {
      if (state === "live") { workspaces = list; workspacesStale = false; live(); }
      else if (state === "ended" || state === "error") {
        // A passing end keeps the last headings (under the banner) while it waits; a final
        // one shows the sessions ungrouped rather than under headings that no longer update.
        if (CS.passing(detail || state)) { workspacesStale = true; again(); }
        else { workspaces = []; workspacesStale = false; }
      }
      renderList();
    });
  });
  if (triage) {
    CS.subscribe("queryContext", {match: {type: "bug"}, sort: ["status"]}, 15, function (m) {
      if (m.ok && m.answer && Array.isArray(m.answer.rows)) { bugs = m.answer.rows; bugError = null; }
      else bugError = m.error || "failed";
      renderTriage();
    });
    retrying(function (live, again) {
      return CS.onActions(function (rows, state, detail) {
        if (state === "live") { activity = rows; activityStale = false; live(); }
        else { activityStale = true; if ((state === "ended" || state === "error") && CS.passing(detail || state)) again(); }
        renderTriage();
      });
    });
  }
  CS.onPresence(function (p) { if (p.mac === undefined) return; mac = p.mac; renderStatus(); renderList(); });
  function awaitShell() {
    CS.ready().then(function (info) { ownerControl = info.control; renderRole(); if (triage) renderTriage(); }).catch(function () {
      var text = "Live terminals work in the published dashboard.";
      if (triage) { text = "Live bug triage works in the published dashboard."; bugs = []; bugError = "no_shell"; renderTriage(); }
      if ((mission || triage) && $("role").textContent !== text) $("role").textContent = text;
      if (!listsLive && agentsState !== "no_shell") { agentsState = "no_shell"; renderList(); }
      // A slow shell may still say hello after ready's three-second timeout.
      // ready's deadline is measured from startup, so later attempts need their own delay.
      // One timer at a time; no network request and no repeated announcement.
      setTimeout(awaitShell, 3000);
    });
  }
  awaitShell();
})();
