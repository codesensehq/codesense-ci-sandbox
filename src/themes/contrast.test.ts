// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { contrastRatio, CONTRAST_THRESHOLDS } from "../shared/lib/color-contrast";
import { BUNDLED_THEMES, themeTokens, type AppTheme } from "./index";
import { kitRoles } from "./kit";

const themes = (Object.keys(BUNDLED_THEMES) as AppTheme[]).flatMap((id) =>
  (["light", "dark"] as const).map((mode) => [id, mode] as const),
);

it.each(themes)("%s/%s control borders contrast with the raised viewer surface", (id, mode) => {
  const tokens = themeTokens(id, mode);
  const roles = kitRoles("viewer");
  expect(
    contrastRatio(tokens[`--theme-${roles["border-control"]}`], tokens[`--theme-${roles.surface}`]),
  ).toBeGreaterThanOrEqual(CONTRAST_THRESHOLDS.nonText);
});
