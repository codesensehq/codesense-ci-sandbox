// SPDX-License-Identifier: MIT
import { create } from "zustand";

/** Frontend-owned presentation inputs; the sessions projection publishes them to every pane. */
export const useAgentListFacts = create<{
  sessions: Record<string, unknown>;
  pane: Record<string, unknown>;
}>(() => ({ sessions: {}, pane: {} }));
