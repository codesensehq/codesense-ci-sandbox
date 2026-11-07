// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

/**
 * `value`, kept for `ms` after it goes away (`leaving` meanwhile), so its
 * element can fade out before it unmounts. Under reduced motion the CSS
 * zeroes the fade; the element still goes once `ms` has passed.
 */
export function useLingering<T>(
  value: T | undefined,
  ms: number,
): { value: T | undefined; leaving: boolean } {
  const [last, setLast] = useState<T | undefined>(value);
  if (value !== undefined && value !== last) setLast(value);
  useEffect(() => {
    if (value !== undefined || last === undefined) return;
    const id = setTimeout(() => setLast(undefined), ms);
    return () => clearTimeout(id);
  }, [value, last, ms]);
  return value !== undefined
    ? { value, leaving: false }
    : { value: last, leaving: last !== undefined };
}
