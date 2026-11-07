// SPDX-License-Identifier: MIT
import { create } from "zustand";

export const useRoleEditorStore = create<{
  teamKey: string | null;
  role: string | null;
  dirty: boolean;
  pendingLeave: (() => void) | null;
  requestLeave: (go: () => void) => void;
  confirmLeave: () => void;
  cancelLeave: () => void;
}>((set, get) => ({
  teamKey: null,
  role: null,
  dirty: false,
  pendingLeave: null,
  requestLeave: (go) => {
    if (get().dirty) set({ pendingLeave: go });
    else go();
  },
  confirmLeave: () => {
    const go = get().pendingLeave;
    set({ dirty: false, pendingLeave: null });
    go?.();
  },
  cancelLeave: () => set({ pendingLeave: null }),
}));

export async function openRoleEditor(teamKey: string, role: string) {
  const { switchView } = await import("../components/agents-tab-nav");
  const { openAgentsTab } = await import("../agents-virtual-tab");
  useRoleEditorStore.getState().requestLeave(() => {
    useRoleEditorStore.setState({ teamKey, role });
    switchView("roles");
    openAgentsTab();
  });
}
