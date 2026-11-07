// SPDX-License-Identifier: MIT
import { createPtyInputBuffer, type PtyInputBuffer } from "@/shared/lib/pty-input-buffer";
import {
  createContextLandingGate,
  type ContextLandingGate,
} from "@/shared/lib/context-landing-gate";

/**
 * A terminal session's input path — the context-landing gate in front of the
 * pre-attach buffer — owned per session, not per pane. A pane that moves (the
 * tab grid reshaping, one tab ⇄ all tabs) unmounts and mounts again at once;
 * the moved pane takes over the same path, so a keystroke typed before the
 * attach settled, or a command held for the worktree's context, survives the
 * move. The path is torn down only when no pane takes it back.
 */

export interface SessionInputHooks {
  onHold?: () => void;
  onTimeout?: () => void;
  onDiscard?: () => void;
}

export interface SessionInput {
  /** Type into the session (held until the context lands and the attach settles). */
  write(data: string): void;
  /** The attach settled; `false` discards held input (nothing to send it to). */
  markReady(flushPending?: boolean): void;
  setLanding(landing: boolean): void;
  /** This pane is done with the path. */
  release(): void;
}

interface Entry {
  buffer: PtyInputBuffer;
  gate: ContextLandingGate;
  hooks: SessionInputHooks;
  holders: number;
  teardown: ReturnType<typeof setTimeout> | null;
}

const entries = new Map<string, Entry>();

/** Take `key`'s input path (`send` delivers to the session), creating it on first
 *  use. `hooks` go to the pane holding it now. */
export function acquireSessionInput(
  key: string,
  send: (data: string) => void,
  hooks: SessionInputHooks,
): SessionInput {
  let entry = entries.get(key);
  if (!entry) {
    const buffer = createPtyInputBuffer(send);
    const created: Entry = {
      buffer,
      hooks,
      holders: 0,
      teardown: null,
      gate: createContextLandingGate({
        send: (data) => buffer.write(data),
        onHold: () => created.hooks.onHold?.(),
        onTimeout: () => created.hooks.onTimeout?.(),
        onDiscard: () => created.hooks.onDiscard?.(),
      }),
    };
    entry = created;
    entries.set(key, entry);
  }
  if (entry.teardown) {
    clearTimeout(entry.teardown);
    entry.teardown = null;
  }
  entry.hooks = hooks;
  entry.holders += 1;
  const held = entry;
  let released = false;
  return {
    write: (data) => {
      if (!released) held.gate.write(data);
    },
    markReady: (flushPending) => {
      if (!released) held.buffer.markReady(flushPending);
    },
    setLanding: (landing) => {
      if (!released) held.gate.setLanding(landing);
    },
    release: () => {
      if (released) return;
      released = true;
      held.holders -= 1;
      if (held.holders > 0) return;
      // A pane that moved mounts again in the same commit: let it take the path back.
      held.teardown = setTimeout(() => {
        if (held.holders > 0 || entries.get(key) !== held) return;
        entries.delete(key);
        held.buffer.dispose();
        held.gate.dispose();
      }, 0);
    },
  };
}
