// SPDX-License-Identifier: MIT
/** A deliberate choice follows its identity through live updates. Without a
 *  current choice (or after it disappears), highlight the first shown item. */
export function selectedIndexById<T>(
  items: readonly T[],
  selectedId: string | null,
  idOf: (item: T) => string,
): number {
  return Math.max(
    0,
    items.findIndex((item) => idOf(item) === selectedId),
  );
}
