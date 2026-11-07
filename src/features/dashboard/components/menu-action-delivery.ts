// SPDX-License-Identifier: MIT
/** Architecture: confirm that the page established its unread hold before the write can refresh it. */
export function deliverMenuAction(
  frame: { contentWindow: Window | null; src: string },
  message: Record<string, unknown>,
  signal: AbortSignal,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const id = crypto.randomUUID();
    const origin = new URL(frame.src).origin;
    const finish = (error?: Error) => {
      clearTimeout(timer);
      window.removeEventListener("message", receive);
      signal.removeEventListener("abort", abort);
      if (error) reject(error);
      else resolve();
    };
    const abort = () => finish(new Error("Menu frame closed"));
    const receive = (event: MessageEvent) => {
      if (
        event.source === frame.contentWindow &&
        event.origin === origin &&
        event.data?.type === "codesense:menu-action-ack" &&
        event.data.id === id
      )
        finish();
    };
    const timer = setTimeout(() => finish(new Error("Inbox menu action was not received")), 1000);
    if (signal.aborted) {
      abort();
      return;
    }
    window.addEventListener("message", receive);
    signal.addEventListener("abort", abort, { once: true });
    frame.contentWindow?.postMessage({ ...message, id }, origin);
  });
}
