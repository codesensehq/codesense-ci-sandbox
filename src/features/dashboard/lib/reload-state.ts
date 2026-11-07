// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";

const RELOAD_SNAPSHOT_TIMEOUT_MS = 100;
const RELOAD_SNAPSHOT_BYTES = 64 * 1024;

/** One authenticated document's optional state; never part of the swap's readiness gate. */
export class ReloadStateCapture {
  private serial = 0;
  private pending?: {
    id: number;
    timer: ReturnType<typeof setTimeout>;
    finish: (state: unknown) => void;
  };

  constructor(
    private post: (message: Record<string, unknown>) => void,
    private document: string,
  ) {}

  capture(): Promise<unknown> {
    this.cancel();
    const id = ++this.serial;
    return new Promise((finish) => {
      const timer = setTimeout(
        () => this.receive(id, null, "beforeReload snapshot exceeded 100 ms; dropped"),
        RELOAD_SNAPSHOT_TIMEOUT_MS,
      );
      this.pending = { id, timer, finish };
      this.post({ type: "codesense:before-reload", id });
    });
  }

  receive(id: number, state: unknown, warning?: string) {
    if (this.pending?.id !== id) return;
    if (!warning && state != null) {
      try {
        const json = JSON.stringify(state);
        if (new TextEncoder().encode(json).length > RELOAD_SNAPSHOT_BYTES)
          warning = "beforeReload snapshot exceeded 64 KB; dropped";
        else state = JSON.parse(json);
      } catch {
        warning = "beforeReload snapshot was not JSON; dropped";
      }
    }
    if (warning) {
      logger.warn("pane-reload", JSON.stringify({ document: this.document, reason: warning }));
      this.post({ type: "codesense:reload-warning", id, warning });
    }
    const pending = this.pending;
    this.pending = undefined;
    clearTimeout(pending.timer);
    pending.finish(warning ? null : (state ?? null));
  }

  cancel() {
    if (!this.pending) return;
    clearTimeout(this.pending.timer);
    this.pending.finish(null);
    this.pending = undefined;
  }
}
