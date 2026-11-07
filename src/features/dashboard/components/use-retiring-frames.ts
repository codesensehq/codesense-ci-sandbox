// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  hasLiveFrames,
  retireAfterFlush,
  remintFrames,
  type LiveFrame,
} from "../lib/frame-lifecycle";
import { bridgeTiming, nowMs } from "../lib/bridge-timing";

/** Retain outgoing React children in place while their frame saves finish. */
export function useRetiringFrames<T>(
  values: readonly T[],
  key: (value: T) => string,
  owns: (frame: LiveFrame, value: T) => boolean,
): readonly T[] {
  const [previous, setPrevious] = useState(values);
  const [retained, setRetained] = useState<ReadonlyMap<string, T>>(() => new Map());
  const operations = useRef(new Set<string>());
  const mounted = useRef(true);
  const incoming = new Set(values.map(key));
  const desired = useRef(incoming);
  useLayoutEffect(() => {
    desired.current = incoming;
  });
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  if (previous.length !== values.length || previous.some((value, i) => value !== values[i])) {
    const next = new Map(retained);
    for (const value of previous) {
      if (!incoming.has(key(value)) && hasLiveFrames((f) => owns(f, value)))
        next.set(key(value), value);
    }
    setPrevious(values);
    setRetained(next);
  }
  useEffect(() => {
    for (const [id, value] of retained) {
      if (operations.current.has(id)) continue;
      operations.current.add(id);
      const select = (frame: LiveFrame) => owns(frame, value);
      void retireAfterFlush(select, () => {
        operations.current.delete(id);
        if (!mounted.current) return;
        if (desired.current.has(id)) {
          bridgeTiming(() => `frame-remint reason=retire-reentered key=${id} t=${nowMs()}`);
          remintFrames(select);
        }
        setRetained((old) => {
          const next = new Map(old);
          next.delete(id);
          return next;
        });
      });
    }
  }, [retained, owns]);
  return [
    ...values,
    ...[...retained].filter(([id]) => !incoming.has(id)).map(([, value]) => value),
  ];
}
