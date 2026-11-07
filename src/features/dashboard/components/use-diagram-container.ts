// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";

/**
 * RF-perf-15 (F-2/F-3, DRY-1) / SR-8 — a ref to a viewer container whose
 * contents are written imperatively via `innerHTML` (or morphdom), OUTSIDE
 * React's vdom. React's unmount detaches the host element but does not blank an
 * imperatively-built subtree, so in WKWebView its native backing (render tree /
 * canvas / decoded images) is retained past unmount — the leak RF-perf-15
 * established. This hook owns that single teardown: on unmount it clears
 * `innerHTML`, releasing the backing by construction.
 *
 * The element is captured at mount so the cleanup holds a stable reference even
 * as React tears the tree down.
 *
 * SR-8: generalized from the diagram-only `HTMLDivElement` shape to any
 * `HTMLElement` so `BlockSection`'s imperatively-patched `<section>` — the one
 * imperative container RF-perf-15 did not cover — opts into the same discipline.
 * `useDiagramContainer` remains as the diagram-specialized alias (unchanged
 * call-sites). Renderer-specific orphan cleanup (mermaid's detached measuring
 * node) stays the render effect's own concern — see `removeMermaidOrphan` — so
 * this hook stays a single, generic innerHTML release.
 *
 * Non-blocking: cleanup is O(1) synchronous DOM work on an already-unmounting
 * node — never on a visible surface, never on an interaction path. The teardown
 * lives in an empty-dep effect, so it fires ONLY on unmount — never on an
 * in-place re-render (which would defeat morphdom's flash-free patch, F.34).
 */
export function useSelfClearingRef<
  T extends HTMLElement = HTMLDivElement,
>(): React.RefObject<T | null> {
  const ref = useRef<T>(null);

  useEffect(() => {
    const el = ref.current;
    return () => {
      if (el) el.innerHTML = "";
    };
  }, []);

  return ref;
}

/**
 * Diagram-specialized alias of {@link useSelfClearingRef}. Kept as a distinct
 * named export so the mermaid/D2 call-sites read intent-first and stay
 * behavior-identical to RF-perf-15.
 */
export function useDiagramContainer(): React.RefObject<HTMLDivElement | null> {
  return useSelfClearingRef<HTMLDivElement>();
}

/**
 * Remove the detached measuring node `mermaid.render(id, …)` can leave behind.
 * Mermaid appends a temporary element id'd `d<id>` (and, in some failure paths,
 * `<id>` itself) to the document to measure text; on certain paths it survives
 * the render. Given the id we used for that render, drop whichever leaked.
 * A `null` id (render never ran) is a no-op.
 */
export function removeMermaidOrphan(id: string | null): void {
  if (!id) return;
  for (const candidate of [id, `d${id}`]) {
    document.getElementById(candidate)?.remove();
  }
}
