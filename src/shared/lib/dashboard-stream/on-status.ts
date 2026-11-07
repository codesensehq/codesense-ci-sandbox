// SPDX-License-Identifier: MIT
import { useDashboardStreamStore } from "@/shared/stores/dashboard-stream-store";
import type { OnStatus } from "./types";

/**
 * SW-DX-3 — the in-app subscription verb (AC-3) for agent session/story deltas.
 * A thin wrapper over the hub's `subscribe`. (Doc views and HTML dashboards
 * query the context engine via `subscribeContext`.)
 *
 *   const off = onStatus({ kind: "story", storyId }, (deltas, { fresh }) => render(deltas, fresh));
 *   // later: off();
 */
export const onStatus: OnStatus = (scope, cb) =>
  useDashboardStreamStore.getState().subscribe(scope, cb);
