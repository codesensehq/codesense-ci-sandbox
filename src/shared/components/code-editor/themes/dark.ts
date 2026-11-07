// SPDX-License-Identifier: MIT
import type { Extension } from "@codemirror/state";
import type { CmModule } from "../cm-loader";
import { buildCodesenseTheme } from "./build-theme";

export function darkTheme(cm: CmModule): Extension {
  return buildCodesenseTheme(cm, true);
}
