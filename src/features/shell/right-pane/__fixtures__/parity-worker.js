/* global self */
// The parity pane's worker: infers which stories are ready for review, off the
// pane's main thread. Served from the same panes/ dir as the pane.
self.onmessage = function (e) {
  var ready = (e.data || []).filter(function (s) {
    return s.status === "review";
  }).length;
  self.postMessage({ ready: ready });
};
