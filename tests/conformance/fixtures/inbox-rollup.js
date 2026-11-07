/* global document, window */
(function () {
  var cs = window.codesense;
  var rows = document.getElementById("rows");
  function load() {
    cs.inboxRollup().then(function (rollup) {
      rows.replaceChildren.apply(
        rows,
        rollup.messages.map(function (message) {
          var item = document.createElement("li");
          item.textContent = message.text;
          return item;
        }),
      );
      cs.ready();
    });
  }
  if (!window.ONCE_READ) cs.onInboxChanged(window.FLAW ? function () {} : load);
  load();
})();
