// SPDX-License-Identifier: MIT
import { mixHex } from "../shared/lib/color-contrast";
import { SURFACES } from "./compiler";

/** One opaque palette-derived pair for the runtime badge, shared by generated and live themes. */
export function runtimeBadgeTokens(tokens: Record<string, string>): Record<string, string> {
  const hue = tokens["--theme-runtime-hermes-text"];
  const result: Record<string, string> = {};
  for (const surface of SURFACES) {
    result[`--theme-runtime-hermes-${surface}-bg`] = mixHex(
      tokens[`--theme-surface-${surface}-bg`],
      hue,
      0.08,
    );
    result[`--theme-runtime-hermes-${surface}-text`] = mixHex(
      hue,
      tokens[`--theme-text-${surface}-primary`],
      0.16,
    );
  }
  return result;
}
