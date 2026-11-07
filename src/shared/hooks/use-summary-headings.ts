// SPDX-License-Identifier: MIT
import { useViewerStore, DEFAULT_SUMMARY_HEADINGS } from "@/shared/stores/viewer-store";

/**
 * SW-EW-32: the configured summary-section headings (first match wins) with the
 * default fallback applied. The single read-site shared by the story HUD and the
 * settings row — so the
 * `?? DEFAULT_SUMMARY_HEADINGS` fallback lives in exactly one place.
 *
 * Returns a referentially-stable array: the store hands back the same reference
 * until the setting changes, and the default is a module constant.
 */
export function useSummaryHeadings(): readonly string[] {
  return useViewerStore((s) => s.settings.summaryHeadings ?? DEFAULT_SUMMARY_HEADINGS);
}
