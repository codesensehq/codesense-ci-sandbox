/* global document, window, location, setTimeout */
// A conforming test dashboard. `window.FLAW` turns on exactly one non-conformance, so each
// check's self-test fails for its own reason.
(function () {
  var flaw = window.FLAW || "";
  var cs = window.codesense;
  var topic = window.TOPIC || "sessions";
  var retried = false;
  var discardSnapshots = false;
  var rows = document.getElementById("rows");
  var status = document.getElementById("status");
  var refresh = document.getElementById("refresh");
  function render(snapshot) {
    if (discardSnapshots || flaw === "team-switch-unrendered") return;
    rows.replaceChildren.apply(
      rows,
      snapshot.snapshot
        .filter(function (row) {
          return topic !== "inbox" || row.rowType === "message";
        })
        .map(function (row) {
          var item = document.createElement("li");
          item.textContent =
            topic === "inbox"
              ? row.message.text
              : window.RENDER_HANDLE
                ? row.handle
                : (row.rowNames && row.rowNames.primary) || row.label || row.id;
          return item;
        }),
    );
  }
  function denied(error) {
    if (flaw === "budget-silent") {
      cs.ready();
      return;
    }
    if (flaw === "budget") {
      status.textContent = "Paused: " + error.message;
      cs.ready();
      setTimeout(load, 200);
      return;
    }
    if ((flaw === "budget-once" || flaw === "budget-delayed") && !retried) {
      retried = true;
      setTimeout(load, window.RETRY_DELAY_MS || (flaw === "budget-delayed" ? 3500 : 120));
    }
    status.textContent = "Paused: " + error.message;
    cs.ready();
  }
  function load() {
    if (flaw === "budget-once" || flaw === "budget-delayed" || flaw === "budget-in-flight") {
      cs.setPrefs({ collapsed: false }).then(function () {
        cs.ready();
      }, denied);
      return;
    }
    cs.subscribe(topic, { collection: window.COLLECTION, onChange: render })
      .then(function (snapshot) {
        render(snapshot);
        return cs.setPrefs({ collapsed: false });
      })
      .then(function () {
        cs.ready();
      }, denied);
  }
  if (flaw === "focus-in") refresh.remove();
  if (flaw === "reparent") status.textContent = "Loaded " + Math.random().toString(36).slice(2);
  if (flaw === "escape")
    window.addEventListener("keydown", function (event) {
      if (event.key === "Escape") event.preventDefault();
    });
  cs.onFlush(function () {
    return flaw === "flush" ? new Promise(function () {}) : Promise.resolve();
  });
  if (flaw === "team-switch")
    cs.onVisibility(function (visible) {
      if (!visible) cs.unsubscribe(topic);
    });
  if (flaw === "team-switch-discard" || flaw === "team-switch-session-stale")
    cs.onVisibility(function (visible) {
      if (!visible) discardSnapshots = true;
    });
  if (flaw === "maximize")
    cs.onSlotContext(function (context) {
      if (context.presentation === "overlay" && !location.hash) {
        location.hash = "reloaded";
        location.reload();
      }
    });
  if (flaw === "axe") refresh.textContent = "";
  refresh.addEventListener("click", load);
  if (window.AUXILIARY_TOPIC)
    cs.subscribe(window.AUXILIARY_TOPIC, { onChange: function () {} }).catch(denied);
  if (flaw === "team-switch-mixed-stale") {
    var inboxRows = document.createElement("ul");
    document.querySelector("main").appendChild(inboxRows);
    // Deliberately draw only the initial snapshot; the live subscription has no onChange.
    cs.subscribe("inbox").then(function (snapshot) {
      snapshot.snapshot
        .filter(function (row) {
          return row.rowType === "message";
        })
        .forEach(function (row) {
          var item = document.createElement("li");
          item.textContent = row.message.text;
          inboxRows.appendChild(item);
        });
    }, denied);
  }
  if (flaw === "team-switch-session-stale") {
    var senders = document.createElement("ul");
    document.querySelector("main").appendChild(senders);
    function renderSenders(snapshot) {
      senders.replaceChildren.apply(
        senders,
        snapshot.snapshot
          .filter(function (row) {
            return row.rowType === "message";
          })
          .map(function (row) {
            var item = document.createElement("li");
            item.textContent = row.message.from + " " + row.message.text;
            return item;
          }),
      );
    }
    cs.subscribe("inbox", { onChange: renderSenders }).then(renderSenders, denied);
  }
  load();
  if (flaw === "budget-in-flight") load();
})();
