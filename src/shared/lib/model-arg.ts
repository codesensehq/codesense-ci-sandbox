// SPDX-License-Identifier: MIT
/** The model an agent's args name: the value of the first `--model <v>`, `--model=<v>` or
 *  `-m <v>`, else null (the CLI's own default). */
export function readModelArg(
  args: readonly string[],
  occurrence: "first" | "last" = "first",
): string | null {
  let found: string | null = null;
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    const value = arg.startsWith("--model=")
      ? arg.slice("--model=".length)
      : arg === "--model" || arg === "-m"
        ? args[++i]
        : undefined;
    if (value !== undefined) {
      found = value || null;
      if (occurrence === "first") return found;
    }
  }
  return found;
}

/** `args` with the model set to `model`: the first model flag is rewritten in the form it was
 *  written, or `--model <model>` is appended when there is none. An empty or null `model`
 *  removes every model flag (the CLI default). Every other arg is kept as it is. */
export function setModelArg(args: readonly string[], model: string | null): string[] {
  const value = model?.trim() ?? "";
  const out: string[] = [];
  let written = false;
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    const inline = arg.startsWith("--model=");
    const split = arg === "--model" || arg === "-m";
    if (!inline && !split) {
      out.push(arg);
      continue;
    }
    if (split) i += 1;
    if (!value || written) continue;
    written = true;
    out.push(...(inline ? [`--model=${value}`] : [arg, value]));
  }
  if (value && !written) out.push("--model", value);
  return out;
}
