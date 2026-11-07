// SPDX-License-Identifier: MIT
import { VERB_REGISTRY } from "../lib/verb-registry.generated";
type Request = {
  jsonrpc?: unknown;
  id?: unknown;
  method?: unknown;
  params?: { name?: unknown; arguments?: unknown };
};
/** MCP Apps stable protocol: the same admission path as the page shim. */
export async function mcpAppsReply(
  request: Request,
  run: (verb: unknown, args: unknown) => Promise<unknown>,
  hostContext: Record<string, unknown> = {},
) {
  const id = request.id;
  if (request.jsonrpc !== "2.0" || (typeof id !== "string" && typeof id !== "number")) return null;
  if (request.method === "ui/initialize")
    return {
      jsonrpc: "2.0",
      id,
      result: {
        protocolVersion: "2026-01-26",
        hostInfo: { name: "CodeSense", version: "0.1.0" },
        hostCapabilities: {
          serverTools: {},
          experimental: {
            "codesense/subscribe": { optional: true },
            "codesense/slotContext": { optional: true },
          },
        },
        hostContext: { platform: "desktop", displayMode: "inline", ...hostContext },
      },
    };
  if (request.method !== "tools/call")
    return { jsonrpc: "2.0", id, error: { code: -32601, message: "Method not found" } };
  try {
    const name = request.params?.name;
    const verb = VERB_REGISTRY.find((v) => v.name === name);
    // Unknown tools still enter the native dispatcher, which records not-allowed.
    const result = await run(verb?.name ?? name, request.params?.arguments ?? {});
    return {
      jsonrpc: "2.0",
      id,
      result: {
        content: [{ type: "text", text: JSON.stringify(result) ?? "null" }],
        structuredContent:
          result !== null && typeof result === "object" && !Array.isArray(result)
            ? result
            : { result: result ?? null },
        isError: false,
      },
    };
  } catch (error) {
    return {
      jsonrpc: "2.0",
      id,
      result: {
        content: [{ type: "text", text: error instanceof Error ? error.message : String(error) }],
        isError: true,
      },
    };
  }
}
