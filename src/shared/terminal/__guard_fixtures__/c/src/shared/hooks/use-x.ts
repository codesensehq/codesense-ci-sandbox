// Violates (c): a React hook value-importing xterm (the shape of the hook that
// owned an xterm before the terminal manager did).
import { Terminal } from "@xterm/xterm";

export function useX(): typeof Terminal {
  return Terminal;
}
