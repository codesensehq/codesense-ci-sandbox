// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import {
  terminalManager,
  type DisplayHandle,
  type TerminalManager,
} from "@/shared/terminal/terminal-manager";
import { PANE_SPINNER_DELAY_MS } from "./use-pane-startup";

export interface EvictedRestore {
  /** Say "Restoring session…": the pane is shown and its rebuild is slow. */
  label: boolean;
  /** Bumped by every rebuilt display (a fresh xterm and search addon). */
  generation: number;
}

/**
 * A pane whose hidden display was released under memory pressure. Shown
 * again, the display is rebuilt from the host snapshot: a fast rebuild shows
 * nothing in between; one slower than {@link PANE_SPINNER_DELAY_MS} says
 * "Restoring session…" on the terminal background until it lands.
 */
export function useEvictedRestore(
  sessionId: string,
  visible: boolean,
  onRestored: (handle: DisplayHandle) => void,
  manager: TerminalManager = terminalManager,
): EvictedRestore {
  const [evicted, setEvicted] = useState(false);
  /** Bumped by every eviction; the slow-restore timer fires for one. */
  const [episode, setEpisode] = useState(0);
  const [slowEpisode, setSlowEpisode] = useState(-1);
  const [generation, setGeneration] = useState(0);
  const onRestoredRef = useRef(onRestored);
  useLayoutEffect(() => {
    onRestoredRef.current = onRestored;
  });

  useEffect(
    () =>
      manager.listen(sessionId, {
        evicted: () => {
          setEvicted(true);
          setEpisode((e) => e + 1);
        },
        restored: (handle) => {
          setEvicted(false);
          if (!handle) return;
          setGeneration((g) => g + 1);
          onRestoredRef.current(handle);
        },
        retired: () => setEvicted(false),
      }),
    [manager, sessionId],
  );

  useEffect(() => {
    if (!evicted || !visible) return;
    const timer = setTimeout(() => setSlowEpisode(episode), PANE_SPINNER_DELAY_MS);
    return () => clearTimeout(timer);
  }, [evicted, visible, episode]);

  return { label: evicted && visible && slowEpisode === episode, generation };
}
