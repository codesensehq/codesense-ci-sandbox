// SPDX-License-Identifier: MIT
/**
 * Holds a command typed into a new worktree's terminal until the team context
 * has landed in it, so a fast first `claude` (or `ls -a`) never runs in a
 * folder that doesn't have `CLAUDE.md` / `.claude/` yet.
 *
 * While context is landing, typing still reaches the shell (the user sees the
 * command at the prompt), but Enter and everything after it is held. The held
 * input runs, in order, the moment context lands, or after
 * {@link CONTEXT_HOLD_TIMEOUT_MS} (then `onTimeout` fires so the terminal can
 * say the command ran without context). Ctrl-C while holding drops the held
 * command and passes the Ctrl-C through, so the user can always back out.
 *
 * Pure and timer-driven so it tests without a PTY. It sits in front of the
 * terminal's pre-spawn input buffer; it never drops input except on Ctrl-C or
 * dispose.
 */
export const CONTEXT_HOLD_TIMEOUT_MS = 10_000;

const ENTER = "\r";
const CTRL_C = "\x03";
const ESC = "\x1b";
const PASTE_START = "\x1b[200~";

export interface ContextLandingGate {
  /** Route one chunk of terminal input through the gate. */
  write(data: string): void;
  /** Context started (`true`) or finished (`false`) landing. Finishing flushes
   *  any held input. */
  setLanding(landing: boolean): void;
  /** Whether a command is currently held. */
  isHolding(): boolean;
  /** Terminal teardown: drop held input and stop the timer. */
  dispose(): void;
}

export interface ContextLandingGateOptions {
  send: (data: string) => void;
  /** The first Enter was held (a command is now waiting for context). */
  onHold?: () => void;
  /** The hold timed out and the held command ran without context. */
  onTimeout?: () => void;
  /** A held command was dropped (Ctrl-C): it will never run. */
  onDiscard?: () => void;
  timeoutMs?: number;
}

export function createContextLandingGate({
  send,
  onHold,
  onTimeout,
  onDiscard,
  timeoutMs = CONTEXT_HOLD_TIMEOUT_MS,
}: ContextLandingGateOptions): ContextLandingGate {
  let landing = false;
  // Set once the hold timed out for this landing: stop gating until the next one.
  let gaveUp = false;
  let disposed = false;
  let held: string[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;

  const clearTimer = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };

  const flush = () => {
    clearTimer();
    const out = held;
    held = [];
    for (const chunk of out) send(chunk);
  };

  const hold = (chunk: string) => {
    const first = held.length === 0;
    held.push(chunk);
    if (!first) return;
    onHold?.();
    timer = setTimeout(() => {
      timer = null;
      gaveUp = true;
      flush();
      onTimeout?.();
    }, timeoutMs);
  };

  return {
    write(data) {
      if (disposed) return;
      if (!landing || gaveUp) {
        send(data);
        return;
      }
      if (held.length > 0) {
        const ctrlC = data.indexOf(CTRL_C);
        if (ctrlC !== -1) {
          // Drop the held command, pass the Ctrl-C through, and keep gating
          // whatever follows it in the same chunk.
          clearTimer();
          held = [];
          onDiscard?.();
          send(data.slice(0, ctrlC + 1));
          const rest = data.slice(ctrlC + 1);
          if (rest) this.write(rest);
          return;
        }
        held.push(data);
        return;
      }
      const enter = data.indexOf(ENTER);
      if (enter === -1) {
        send(data);
        return;
      }
      // A bracketed paste is one unit: splitting it would leave the shell in
      // paste mode. Hold it whole.
      if (data.includes(PASTE_START)) {
        hold(data);
        return;
      }
      // Keep Alt/Meta+Enter (ESC CR) together.
      const cut = enter > 0 && data[enter - 1] === ESC ? enter - 1 : enter;
      if (cut > 0) send(data.slice(0, cut));
      hold(data.slice(cut));
    },
    setLanding(next) {
      if (disposed || next === landing) return;
      landing = next;
      if (next) {
        gaveUp = false;
        return;
      }
      flush();
    },
    isHolding: () => held.length > 0,
    dispose() {
      disposed = true;
      clearTimer();
      held = [];
    },
  };
}
