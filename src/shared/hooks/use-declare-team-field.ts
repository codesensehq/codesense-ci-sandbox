// SPDX-License-Identifier: MIT
import { useCallback } from "react";

import {
  declareTeamField,
  type FieldSchema,
  type FieldType,
  type NewFieldInput,
} from "@/tauri/commands";

/** SW-AU-77E: the shape a caller hands the declare hook. `type` is one of the five
 *  authoring types; `allowedValues` is required for `choice`. `value` (optional) is the
 *  value about to be set on the current story — the backend validates it before writing. */
export interface DeclareFieldArgs {
  name: string;
  type: FieldType;
  allowedValues?: string[];
  required?: boolean;
  value?: string | null;
}

/**
 * SW-AU-77E · The seam that declares a typed team field from the metadata drawer
 * (declare-by-use). It calls the additive, RMW-safe `declare_team_field` (never the
 * full-vector `save_team_config` overwrite) and returns the command's AUTHORITATIVE
 * reloaded schema.
 *
 * Returns an async `declare` — the caller wraps it in `useSubmitAction` (or equivalent)
 * for the toast + disabled-while-submitting affordance. All work is off the webview
 * thread inside the Tauri command; this hook does no render-blocking work (AC-9).
 */
export function useDeclareTeamField(orgRoot: string, team: string) {
  return useCallback(
    async (args: DeclareFieldArgs): Promise<FieldSchema> => {
      const field: NewFieldInput = {
        name: args.name,
        type: args.type,
        allowedValues: args.allowedValues ?? [],
        required: args.required ?? false,
        value: args.value ?? null,
      };
      return declareTeamField(orgRoot, team, field);
    },
    [orgRoot, team],
  );
}
