// SPDX-License-Identifier: MIT
import { create } from "zustand";

/**
 * The shell's right-column host element for each team, keyed by teamKey. The workbench renders
 * one host per opened team and registers it through a callback ref; the team view renders its
 * right pane into its own host through a portal, so the pane keeps its React state and context
 * (the story launch provider included) across team switches, as it did inside the team view.
 */
interface RightPaneHostRegistry {
  hosts: Record<string, HTMLElement | undefined>;
}

export const useRightPaneHostRegistry = create<RightPaneHostRegistry>(() => ({ hosts: {} }));

/** Register `el` as `teamKey`'s right-pane host; returns the unregister cleanup (a React 19
 *  ref-callback cleanup), which only clears its own registration. */
export function registerRightPaneHost(teamKey: string, el: HTMLElement): () => void {
  useRightPaneHostRegistry.setState((s) => ({ hosts: { ...s.hosts, [teamKey]: el } }));
  return () => {
    if (useRightPaneHostRegistry.getState().hosts[teamKey] !== el) return;
    useRightPaneHostRegistry.setState((s) => {
      const hosts = { ...s.hosts };
      delete hosts[teamKey];
      return { hosts };
    });
  };
}

/** `teamKey`'s right-pane host, reactive; undefined until the workbench has drawn it. */
export function useRightPaneHost(teamKey: string): HTMLElement | undefined {
  return useRightPaneHostRegistry((s) => s.hosts[teamKey]);
}
