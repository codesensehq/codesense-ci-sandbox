// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * The CLI's terminal section element: one, in the shell's workbench. The workbench registers it
 * through a callback ref; surfaces laid out against the terminal (the full-screen inbox ends at
 * its split) subscribe, so they re-measure when the section mounts, unmounts or is replaced.
 */
interface TerminalSectionRegistry {
  section: HTMLElement | undefined;
}

export const useTerminalSectionRegistry = create<TerminalSectionRegistry>(() => ({
  section: undefined,
}));

/** Register `el` as the terminal section; returns the unregister cleanup (a React 19
 *  ref-callback cleanup), which only clears its own registration. */
export function registerTerminalSection(el: HTMLElement): () => void {
  useTerminalSectionRegistry.setState({ section: el });
  return () => {
    if (useTerminalSectionRegistry.getState().section !== el) return;
    useTerminalSectionRegistry.setState({ section: undefined });
  };
}

/** The registered terminal section, reactive. */
export function useTerminalSection(): HTMLElement | undefined {
  return useTerminalSectionRegistry((s) => s.section);
}
