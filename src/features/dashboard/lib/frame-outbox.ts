// SPDX-License-Identifier: MIT
type Endpoint = { contentWindow: Window | null; src: string };

/** One document's host messages wait for its authenticated shim, never about:blank. */
export class FrameOutbox {
  private document: string | null = null;
  private connected = false;
  private pending: Array<Record<string, unknown>> = [];

  begin(document: string | null) {
    if (this.document === document) return;
    this.document = document;
    this.connected = false;
    this.pending = [];
  }

  post(frame: Endpoint | null, message: Record<string, unknown>) {
    if (!this.connected || !frame?.contentWindow) this.pending.push(message);
    else this.deliver(frame, message);
  }

  ready(frame: Endpoint | null) {
    if (!frame?.contentWindow || !frame.src) return;
    this.connected = true;
    const pending = this.pending;
    this.pending = [];
    for (const message of pending) this.deliver(frame, message);
  }

  private deliver(frame: Endpoint, message: Record<string, unknown>) {
    frame.contentWindow?.postMessage(message, new URL(frame.src).origin);
  }
}
