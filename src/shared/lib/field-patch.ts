// SPDX-License-Identifier: MIT
/** JSON merge patch: objects merge by field, arrays replace, null removes a field. */
export function fieldPatch(before: unknown, after: unknown): unknown {
  if (JSON.stringify(before) === JSON.stringify(after)) return undefined;
  if (after === null || typeof after !== "object" || Array.isArray(after)) return after;
  const previous =
    before && typeof before === "object" && !Array.isArray(before)
      ? (before as Record<string, unknown>)
      : {};
  const next = after as Record<string, unknown>;
  return Object.fromEntries(
    [...new Set([...Object.keys(previous), ...Object.keys(next)])].flatMap((key) => {
      const patch = key in next ? fieldPatch(previous[key], next[key]) : null;
      return patch === undefined ? [] : [[key, patch]];
    }),
  );
}

export function applyFieldPatch(before: unknown, patch: unknown): unknown {
  if (!patch || typeof patch !== "object" || Array.isArray(patch)) return patch;
  const result = {
    ...(before && typeof before === "object" && !Array.isArray(before) ? before : {}),
  } as Record<string, unknown>;
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete result[key];
    else result[key] = applyFieldPatch(result[key], value);
  }
  return result;
}
