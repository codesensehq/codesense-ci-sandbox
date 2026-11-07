// SPDX-License-Identifier: MIT
import { useEffect, useMemo } from "react";
import { create } from "zustand";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { useInboxProjection } from "./use-inbox-projection";
import { joinInboxSessions } from "../projection";
import { loadInboxSessions } from "../api";
import { logger } from "@/shared/lib/logger";
import type { SessionListRow } from "@/tauri/commands";

interface SessionsState {
  sessions: SessionListRow[] | null;
  load: () => Promise<void>;
}
export const useInboxSessionsStore = create<SessionsState>((set) => ({
  sessions: null,
  load: async () => {
    try {
      set({ sessions: await loadInboxSessions() });
    } catch (error) {
      logger.info("inbox", `Session projection read failed: ${String(error)}`);
    }
  },
}));
let watchers = 0;
let stop: (() => void) | null = null;
function watch() {
  if (++watchers === 1) {
    void useInboxSessionsStore.getState().load();
    stop = liveHub.watch("sessions", "all", (snapshot) => {
      useInboxSessionsStore.setState({
        sessions: snapshot.snapshot as unknown as SessionListRow[],
      });
    });
  }
  return () => {
    if (--watchers === 0) {
      stop?.();
      stop = null;
    }
  };
}
export function useInboxSessions(): SessionListRow[] | null {
  useEffect(watch, []);
  const sessions = useInboxSessionsStore((s) => s.sessions);
  const counts = useInboxProjection();
  return useMemo(() => (sessions ? joinInboxSessions(sessions, counts) : null), [sessions, counts]);
}
