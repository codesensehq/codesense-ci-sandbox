// SPDX-License-Identifier: MIT
import { isTerminalServerRefusal } from "@/features/shell/lib/terminal-server";
import { isLiveStreamCurrent } from "@/shared/terminal/live-stream";
import type { LiveStream } from "@/shared/terminal/live-stream";
import type { TerminalSnapshot } from "@/features/viewer/lib/cli-workspace/transport/terminal-session-source";

/**
 * The FE viewport attach model. A terminal display exists only for a tab its
 * host already placed — its session was opened (spawned and placed at once)
 * before, and a restored tab's session is still running or has exited with its
 * scrollback kept — so a display only ever ATTACHES: it opens a live stream
 * (listen, then subscribe: snapshot + live events), and DETACHES (unsubscribe)
 * when it is disposed, never kills. The TerminalManager is the only caller.
 */

/** Injectable IPC surface — the manager passes the real stream opener; tests
 *  pass mocks. */
export interface AttachDeps {
  /** Open the session's live stream. Rejects with "session not found" when the
   *  host holds no such session, or with the transport's error. */
  open: (sessionId: string) => Promise<LiveStream>;
  /** True when the display was disposed mid-await — abort and clean up. */
  isAborted: () => boolean;
}

export type AttachResult =
  | {
      attached: true;
      /** Adopt once the snapshot is written; close on unmount. */
      stream: LiveStream;
      /** The host emulator's terminal: the display is built from it. */
      snapshot: TerminalSnapshot;
    }
  /** The host no longer holds the session (it ended and was freed): the pane
   *  shows a legible "session ended" state — never a fresh shell. */
  | { attached: false; ended: true }
  /** The host could not be asked (IPC, the terminal server, no reply in time):
   *  the session may be fine, so the pane offers Retry rather than calling it
   *  ended. `incompatible`: the terminal server runs another version and
   *  refused; only restarting it helps. */
  | { attached: false; ended: false; incompatible: boolean; error: string };

/** The host's answer for a session it does not hold. Kept byte-identical to
 *  `SESSION_NOT_FOUND` in `src-tauri/crates/codesense-pty-core/src/domain/types.rs`
 *  (and the mock backend's `subscribe_terminal`); any other error is a transport
 *  failure. */
export const SESSION_NOT_FOUND = "session not found";

/** How long an attach waits for the host's snapshot before it gives up. */
export const ATTACH_TIMEOUT_MS = 5_000;

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const TIMED_OUT = Symbol("timed out");

/**
 * Attach a viewport to `sessionId`. Returns `null` when the display was disposed
 * mid-flight (any viewport it registered is already detached). A stream that
 * opens after the attach gave up waiting is closed at once.
 */
export async function attachTerminal(
  sessionId: string,
  deps: AttachDeps,
): Promise<AttachResult | null> {
  const opening = deps.open(sessionId);
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<typeof TIMED_OUT>((resolve) => {
    timer = setTimeout(() => resolve(TIMED_OUT), ATTACH_TIMEOUT_MS);
  });
  let stream: LiveStream | typeof TIMED_OUT;
  try {
    stream = await Promise.race([opening, timeout]);
  } catch (e) {
    if (deps.isAborted()) return null;
    const error = errorText(e);
    if (error.includes(SESSION_NOT_FOUND)) return { attached: false, ended: true };
    return { attached: false, ended: false, incompatible: isTerminalServerRefusal(error), error };
  } finally {
    clearTimeout(timer);
  }
  if (stream === TIMED_OUT) {
    opening.then(
      (late) => late.close(),
      () => {},
    );
    if (deps.isAborted()) return null;
    return { attached: false, ended: false, incompatible: false, error: "no snapshot in time" };
  }
  if (deps.isAborted() || !isLiveStreamCurrent(stream)) {
    stream.close();
    return null;
  }
  return { attached: true, stream, snapshot: stream.subscription.snapshot };
}
