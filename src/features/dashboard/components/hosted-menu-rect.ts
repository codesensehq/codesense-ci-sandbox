// SPDX-License-Identifier: MIT
import type { FrameAnchorRect } from "@/features/shell/host-menu-context";

/** The parent owns the child iframe's box; the app cannot measure across origins. */
export function hostedMenuRect(
  parent: HTMLIFrameElement,
  origin: string,
  signal: AbortSignal,
): Promise<FrameAnchorRect | null> {
  return new Promise((resolve) => {
    const id = crypto.randomUUID();
    const finish = (rect: FrameAnchorRect | null) => {
      clearTimeout(timer);
      window.removeEventListener("message", receive);
      signal.removeEventListener("abort", abort);
      resolve(rect);
    };
    const abort = () => finish(null);
    const receive = (event: MessageEvent) => {
      if (
        event.source !== parent.contentWindow ||
        event.origin !== new URL(parent.src).origin ||
        event.data?.type !== "codesense:child-rect-result" ||
        event.data.id !== id
      )
        return;
      finish(event.data.rect ?? null);
    };
    const timer = setTimeout(() => finish(null), 1000);
    if (signal.aborted) {
      finish(null);
      return;
    }
    window.addEventListener("message", receive);
    signal.addEventListener("abort", abort, { once: true });
    parent.contentWindow?.postMessage(
      { type: "codesense:child-rect", id, origin },
      new URL(parent.src).origin,
    );
  });
}
