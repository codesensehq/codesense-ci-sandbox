// SPDX-License-Identifier: MIT
import { useTheme } from "next-themes";

/** Every bundled family supports both modes; surface darkness follows Mode. */
export function useIsLightViewerTheme(): boolean {
  const { resolvedTheme } = useTheme();
  return resolvedTheme !== "dark";
}
