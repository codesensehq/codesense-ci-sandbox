// SPDX-License-Identifier: MIT
/**
 * SW-EW-39: an ordered hold-until-ready queue for terminal INPUT.
 *
 * Keystrokes typed into a terminal while its pane is still attaching — xterm is
 * live but the viewport isn't attached yet — are held in order and flushed, in
 * order, the moment the pane is attached (never a silent drop into a session the
 * pane can't reach yet). A small state machine: holding → ready, plus disposed.
 *
 * SW-FX-51 (defect #3): once ready, rapid keystrokes are COALESCED behind a tiny
 * flush window (`scheduleFlush`, default `queueMicrotask`) so a same-window burst
 * becomes one `send` (one `write_pty` IPC invoke) instead of one per character.
 * A single keystroke on an idle terminal still flushes on the very next window —
 * a microtask, so there is no perceptible echo delay. Coalescing applies ONLY to
 * the ready egress; the pre-ready hold still flushes each held chunk individually
 * and in exact order (the SW-EW-39 guarantee is unchanged). `dispose` drops the
 * batch synchronously so nothing is stranded across a teardown.
 *
 * Deliberately NOT `createPtyWriteBuffer` (`shared/terminal`): that one
 * rAF-coalesces OUTPUT into xterm. This one buffers INPUT toward the PTY and
 * gates on session readiness. Pure + dependency-injected (`send`, `scheduleFlush`)
 * so it unit-tests without a real PTY or the microtask queue.
 */
export interface PtyInputBuffer {
  /** Deliver `data` now if the session is ready (coalesced behind the flush
   *  window), else hold it (preserving order) until `markReady`. No-op after
   *  `dispose`. */
  write(data: string): void;
  /** The pane is attached — become ready. By default flush held input in order;
   *  pass `flushPending=false` to DISCARD it instead (the attach failed: there
   *  is no session to send it to). Idempotent while already ready. */
  markReady(flushPending?: boolean): void;
  /** Terminal teardown — drop held input, drop any pending batch, and reject all
   *  future writes. The viewport is gone, so held keystrokes have no live
   *  target and any already-scheduled flush becomes a no-op. */
  dispose(): void;
}

export interface PtyInputBufferOptions {
  /** Schedules a coalesced flush of ready-egress input. Called at most once per
   *  burst. Defaults to `queueMicrotask` — coalesces keystrokes that arrive in
   *  the same synchronous turn (paste/IME/key-repeat) with no perceptible delay.
   *  Injectable so tests drive the flush deterministically. */
  scheduleFlush?: (flush: () => void) => void;
}

export function createPtyInputBuffer(
  send: (data: string) => void,
  options: PtyInputBufferOptions = {},
): PtyInputBuffer {
  const scheduleFlush = options.scheduleFlush ?? ((flush: () => void) => queueMicrotask(flush));

  let ready = false;
  let disposed = false;
  let pending: string[] = [];
  // Ready-egress coalescing buffer + whether a flush is already scheduled for
  // the current burst (so N writes schedule ONE flush).
  let batch = "";
  let flushScheduled = false;

  // Send whatever is coalesced right now, in one invoke. Also clears the
  // scheduled flag so a stale scheduled callback after a sync drain is a no-op.
  const flushBatch = () => {
    flushScheduled = false;
    if (disposed || batch === "") return;
    const out = batch;
    batch = "";
    send(out);
  };

  return {
    write(data: string) {
      if (disposed) return;
      if (!ready) {
        pending.push(data);
        return;
      }
      batch += data;
      if (!flushScheduled) {
        flushScheduled = true;
        scheduleFlush(flushBatch);
      }
    },
    markReady(flushPending = true) {
      if (disposed || ready) return;
      ready = true;
      const queued = pending;
      pending = [];
      // Flush the pre-ready hold in the exact order the user typed, one chunk at
      // a time — the SW-EW-39 guarantee. (Coalescing is for the live egress
      // below, not the replay of held input.)
      if (flushPending) for (const chunk of queued) send(chunk);
    },
    dispose() {
      disposed = true;
      pending = [];
      batch = "";
    },
  };
}
