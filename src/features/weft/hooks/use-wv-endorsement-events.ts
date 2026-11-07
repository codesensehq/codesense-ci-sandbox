// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef } from "react";

import { listen } from "@tauri-apps/api/event";

import {
  WV_ASSERTION_POSTED_EVENT,
  WV_ASSERTION_RATIFIED_EVENT,
  WV_ASSERTION_REVOKED_EVENT,
} from "@/tauri/events";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";

/** The unscoped attention board still follows the assertion lifecycle events. */
export interface WvAssertionPostedPayload {
  id: number;
  sessionId: string;
  claim: string;
  channel: string;
  disposition: string;
  runId: string | null;
}

export interface WvAssertionSettledPayload {
  id: number;
  disposition: string;
  channel?: string;
  claim?: string;
}

export interface UseWvEndorsementEventsOptions {
  onPosted: (payload: WvAssertionPostedPayload) => void;
  onRatified: (id: number) => void;
  onRevoked: (id: number) => void;
}

export function useWvEndorsementEvents(options: UseWvEndorsementEventsOptions): void {
  // Keep the latest callbacks in a ref so the subscribe effect runs once (mount) and
  // never re-subscribes on a caller re-render. Synced in a layout effect — which runs
  // before the diff/subscribe effect in the same commit — rather than during render
  // (the house idiom for a latest-value ref; see live-list.tsx `helpers`).
  const cbs = useRef(options);
  useLayoutEffect(() => {
    cbs.current = options;
  });

  useEffect(() => {
    const group = createListenerGroup();
    group.track(
      listen<WvAssertionPostedPayload>(WV_ASSERTION_POSTED_EVENT, (e) => {
        cbs.current.onPosted(e.payload);
      }),
    );
    group.track(
      listen<WvAssertionSettledPayload>(WV_ASSERTION_RATIFIED_EVENT, (e) => {
        cbs.current.onRatified(e.payload.id);
      }),
    );
    group.track(
      listen<WvAssertionSettledPayload>(WV_ASSERTION_REVOKED_EVENT, (e) => {
        cbs.current.onRevoked(e.payload.id);
      }),
    );
    return () => group.dispose();
  }, []);
}
