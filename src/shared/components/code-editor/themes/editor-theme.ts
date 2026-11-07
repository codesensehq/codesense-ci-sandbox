// SPDX-License-Identifier: MIT
/**
 * Resolve the app's effective light/dark from a `theme` prop and next-themes.
 * `"auto"` follows next-themes; explicit values force-override. Folded here
 * from the copies that lived in `CodeEditor` and `use-merge-view-chrome`.
 */
export function resolveAppTheme(
  themeProp: "light" | "dark" | "auto",
  resolvedTheme: string | undefined,
): "light" | "dark" {
  if (themeProp === "light" || themeProp === "dark") return themeProp;
  return resolvedTheme === "dark" ? "dark" : "light";
}
