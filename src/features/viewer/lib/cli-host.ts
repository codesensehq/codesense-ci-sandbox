// SPDX-License-Identifier: MIT
/**
 * The one CLI's host element, drawn once by the global CLI in the shell's workbench. Every "is
 * focus in the CLI" check and every scan for the CLI's terminals is scoped to it, so a terminal
 * drawn elsewhere (the Agents pane's quick view, a run's output) is never taken for a tile's.
 */
export const CLI_HOST_SELECTOR = "[data-xterm-host]";

/** Where to look for the CLI's terminals: its host, or null before it has mounted. */
export function cliHostRoot(): ParentNode | null {
  return document.querySelector(CLI_HOST_SELECTOR);
}
