// SPDX-License-Identifier: MIT
/**
 * Turn whatever a source-diff fetch rejected with into a sentence a reviewer can
 * act on. Local commands reject with a string; remote ones with the typed
 * `RemoteSourceError` object, which `String(e)` would print as "[object Object]".
 * `subject` names what was being read, so a `notFound` reads right for a whole
 * change list as well as one file.
 */
export function describeDiffError(e: unknown, subject: "file" | "changes" = "file"): string {
  if (e == null || e === "") return "The diff couldn’t be loaded.";
  if (e instanceof Error) return e.message || "The diff couldn’t be loaded.";
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "kind" in e) {
    switch ((e as { kind: unknown }).kind) {
      case "disconnected":
        return "Can’t reach the machine right now. The diff refreshes when it reconnects.";
      case "unauthorized":
        return "You no longer have access to this workspace on the machine.";
      case "notFound":
        return subject === "changes"
          ? "That workspace is no longer on the machine."
          : "That file is no longer in the workspace.";
      case "boundsExceeded":
        return subject === "changes"
          ? "The change list is too large to read from the machine."
          : "This file is over 4 MB — too large to diff from a machine.";
      case "conflict":
        return subject === "changes"
          ? "The workspace changed while it was being read. Retry to load the latest."
          : "The file changed while it was being read. Retry to load the latest.";
      case "transportFailure": {
        const message = (e as { message?: unknown }).message;
        return typeof message === "string" && message ? message : "The machine couldn’t answer.";
      }
    }
  }
  if (e && typeof e === "object") {
    const message = (e as { message?: unknown }).message;
    if (typeof message === "string" && message) return message;
    return "The diff couldn’t be loaded.";
  }
  return String(e);
}

/**
 * The machine's terminal server predates the diff operations — the backend
 * (`remote_source.rs`, `DIFF_NEEDS_SERVER_UPDATE`) words this as an update to
 * make, and the rail turns it into that action rather than a load failure.
 */
export function needsServerUpdate(message: string): boolean {
  return /terminal server is too old/i.test(message);
}
