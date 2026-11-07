// SPDX-License-Identifier: MIT
import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import type { PaneIntent } from "../right-pane/panes/pane-store";
import type { SlotId } from "./slot-registry";
interface NativeSlot {
  panelId: string;
  state: Record<string, unknown>;
  ready: boolean;
  intent?: PaneIntent;
  write: (patch: Record<string, unknown>) => void;
}
const Context = createContext<NativeSlot | null>(null);
export const useNativeSlot = () => useContext(Context);
/** Pages and built-ins subscribe to the same host-owned UI topic; no second persistence. */
export function NativeSlotProvider({
  slotId,
  panelId,
  viewKey,
  intent,
  onIntentDelivered,
  children,
}: {
  slotId: SlotId;
  panelId: string;
  viewKey: string | null;
  intent?: PaneIntent;
  onIntentDelivered?: () => void;
  children: ReactNode;
}) {
  const [state, setState] = useState<Record<string, unknown>>({});
  const [ready, setReady] = useState(false);
  useEffect(() => {
    if (!viewKey) return;
    return liveHub.watch(`ui:${slotId}`, viewKey, (snapshot) => {
      setState(snapshot.snapshot[0] ?? {});
      setReady(true);
    });
  }, [slotId, viewKey]);
  const write = useCallback(
    (patch: Record<string, unknown>) => {
      if (!viewKey) return;
      // Native selection now owns the shared state; do not replay the reveal on retry.
      if (intent) onIntentDelivered?.();
      void liveHub.writeUi(`ui:${slotId}`, viewKey, patch).catch(console.error);
    },
    [slotId, viewKey, intent, onIntentDelivered],
  );
  return (
    <Context.Provider value={{ panelId, state, ready, intent, write }}>{children}</Context.Provider>
  );
}
