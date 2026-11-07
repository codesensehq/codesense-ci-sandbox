// Violates (b): a display terminal constructed outside the manager.
import { Terminal } from "@xterm/xterm";

export const makeTerminal = (): Terminal => new Terminal();
