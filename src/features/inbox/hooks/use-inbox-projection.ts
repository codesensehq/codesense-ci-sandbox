// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { create } from "zustand";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { countRows, type InboxCounts } from "../projection";

export const useInboxProjectionStore = create<{ counts: InboxCounts[] }>()(() => ({ counts: [] }));
let watchers = 0;
let stop: (() => void) | null = null;
function watch() {
  if (++watchers === 1)
    stop = liveHub.watch("inbox", "all", (snapshot) =>
      useInboxProjectionStore.setState({ counts: countRows(snapshot.snapshot) }),
    );
  return () => {
    if (--watchers === 0) {
      stop?.();
      stop = null;
    }
  };
}
export function useInboxProjection(): InboxCounts[] {
  useEffect(watch, []);
  return useInboxProjectionStore((state) => state.counts);
}
