// SPDX-License-Identifier: MIT
import { cancelResilientRaf, resilientRaf } from "@/shared/lib/resilient-raf";

export interface PtyWriteBuffer {
  /** Append data and schedule a frame flush if none is pending. */
  append(data: string): void;
  /** Write everything appended so far now, cancelling the pending frame
   *  (detach, unmount, or before anything else is written to the terminal). */
  flush(): void;
  /** Discard output only when its private authority has retired. */
  discard(): void;
}

/**
 * An animation-frame write coalescer: every `append()` inside one frame reaches
 * `write` as a single call, cutting per-call overhead during heavy output. It
 * only coalesces, it never drops: the frame is scheduled through `resilientRaf`,
 * so a hidden document still flushes (on a timer), and `flush` writes whatever
 * is pending at once.
 */
export function createPtyWriteBuffer(write: (data: string) => void): PtyWriteBuffer {
  let buffer = "";
  let frame = 0;

  const writePending = () => {
    if (!buffer) return;
    try {
      write(buffer);
    } catch {
      // A failed write must not escape the frame callback; keep the bytes so the
      // next flush retries them in order.
      return;
    }
    buffer = "";
  };

  return {
    append(data: string) {
      buffer += data;
      if (frame === 0) {
        frame = resilientRaf(() => {
          frame = 0;
          writePending();
        });
      }
    },
    discard() {
      if (frame !== 0) cancelResilientRaf(frame);
      frame = 0;
      buffer = "";
    },
    flush() {
      if (frame !== 0) {
        cancelResilientRaf(frame);
        frame = 0;
      }
      writePending();
    },
  };
}
