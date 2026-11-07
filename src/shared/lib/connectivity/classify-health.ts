// SPDX-License-Identifier: MIT
import { THIN_THRESHOLD, type HealthVerdict } from "./types";

/**
 * SW-DX-8 — the connectivity-health verdict (AC-4). Keys off the count of
 * RESOLVED FORWARD supporting docs — the story's OWN declared context — NOT
 * backlinks: a story with many backlinks but zero forward docs is still building
 * blind. Zero → `context-desert` ("will be built blind"); below the (small)
 * `thinThreshold` → `thin`; else `healthy`.
 */
export function classifyHealth(
  forwardCount: number,
  thinThreshold: number = THIN_THRESHOLD,
): HealthVerdict {
  if (forwardCount <= 0) return "context-desert";
  if (forwardCount < thinThreshold) return "thin";
  return "healthy";
}
