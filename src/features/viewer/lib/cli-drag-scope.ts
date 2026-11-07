// SPDX-License-Identifier: MIT
import { createContext, useContext, useId } from "react";

/** Droppables stop accepting a drag in the frame before its old-team sensor cancels. A retained
 *  hidden team's grid is stale too: it is not on screen, so it takes no drag. */
export const CliDragStaleContext = createContext(false);
export const useCliDragStale = () => useContext(CliDragStaleContext);

/**
 * The dnd-kit id a CLI tile or drop zone registers under. Retained hidden grids repeat the shown
 * team's ids (one `terminal-grid`, the same workspace ids) inside the Workbench's one
 * `DndContext`, where a registration is keyed by id: a hidden one would replace the shown one and
 * stay behind disabled. A stale grid registers under a private id instead, so only the shown
 * team's grid holds the real ids, and the id grammar the drop resolvers parse is untouched.
 */
export function useCliDndId(id: string): string {
  const stale = useCliDragStale();
  const own = useId();
  return stale ? `stale:${own}:${id}` : id;
}
