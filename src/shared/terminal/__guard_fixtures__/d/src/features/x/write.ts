// Violates (d): bytes written into a terminal outside the manager.
import type { Terminal } from "@xterm/xterm";

export function put(term: Terminal): void {
  term.write("x");
}
