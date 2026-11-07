// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

import type { RunOutcomeRead } from "@/shared/lib/run-outcome";
import { listRunOutcomes } from "@/tauri/commands";

export interface RunOutcomesState {
  outcomes: RunOutcomeRead[] | null;
  error: string | null;
  reload: () => Promise<void>;
}

/** Retained, latest-request-wins history loading for the Runs axis. */
export function useRunOutcomes(active: boolean): RunOutcomesState {
  const [outcomes, setOutcomes] = useState<RunOutcomeRead[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);

  const load = useCallback(async (request: number) => {
    try {
      const next = await listRunOutcomes();
      if (request !== generation.current) return;
      setOutcomes(next);
      setError(null);
    } catch (cause) {
      if (request !== generation.current) return;
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, []);

  const reload = useCallback(async () => {
    if (!active) return;
    await load(++generation.current);
  }, [active, load]);

  useEffect(() => {
    generation.current += 1;
    if (active) {
      const request = generation.current;
      queueMicrotask(() => void load(request));
    }
    return () => {
      generation.current += 1;
    };
  }, [active, load]);

  return { outcomes, error, reload };
}
