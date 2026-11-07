// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  liveHub,
  type HubFrame,
  type HubSnapshot,
  type RunReviewTopic,
} from "@/features/dashboard/lib/live-hub";

import { useLocalProfileStore } from "@/shared/stores/local-profile-store";

export interface RunReviewScope {
  teamKey: string;
  visible?: boolean;
}

/** Native consumers use the window hub's existing frame lifecycle, including stale/reveal. */
export function useRunReview(
  topic: RunReviewTopic,
  runId: string | null,
  storyId: string | null,
  { teamKey, visible = true }: RunReviewScope,
) {
  const principal = useLocalProfileStore((s) => s.status?.active?.principal);
  const key =
    runId && principal
      ? `runReview:${JSON.stringify([principal, teamKey, storyId ?? "", runId])}`
      : null;
  const frame = useRef<HubFrame | null>(null);
  const shown = useRef(visible);
  const [result, setResult] = useState<{ key: string; snapshot?: HubSnapshot; error?: string }>();
  const [attempt, setAttempt] = useState(0);
  const reload = useCallback(() => setAttempt((value) => value + 1), []);

  useLayoutEffect(() => {
    shown.current = visible;
  }, [visible]);

  useEffect(() => {
    if (!key) return;
    let disposed = false;
    const readSnapshot = () => {
      void subscription.subscribe(topic, undefined, { viewKey: key }).then(
        (snapshot) => {
          if (!disposed && shown.current) setResult({ key, snapshot });
        },
        (error: unknown) => {
          if (!disposed && shown.current) setResult({ key, error: String(error) });
        },
      );
    };
    const subscription = liveHub.attach(
      `run-review:${crypto.randomUUID()}`,
      key,
      (message) => {
        if (message.topic === topic) readSnapshot();
      },
      shown.current,
    );
    frame.current = subscription;
    readSnapshot();
    return () => {
      disposed = true;
      frame.current = null;
      subscription.dispose();
    };
  }, [key, topic, attempt]);

  useEffect(() => {
    shown.current = visible;
    frame.current?.visible(visible);
  }, [visible]);

  return {
    snapshot: result?.key === key ? result.snapshot : undefined,
    error: result?.key === key ? result.error : undefined,
    reload,
  };
}
