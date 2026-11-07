// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import {
  useAgentChatStore,
  type ChatRole,
  type RenderTarget,
} from "@/shared/stores/use-agent-chat-store";
import { logger } from "@/shared/lib/logger";
import type { AgentMessagePayload } from "@/shared/lib/agent-handshake";

/**
 * SW-GS-2b (B) — the agent→UI chat channel listener. Mirrors `useMcpActions`:
 * the `post_agent_message` MCP verb emits `mcp:agent-message`; this appends the
 * message to the per-session transcript store. Session-keyed and agent-agnostic
 * (no git-sync/Claude specifics). Mount once at app root.
 */
export function useAgentMessageListener(): void {
  useEffect(() => {
    const unlisten = listen<AgentMessagePayload>("mcp:agent-message", (event) => {
      const { sessionId, text } = event.payload;
      if (!sessionId || !text) return;
      const role: ChatRole = event.payload.role === "user" ? "user" : "agent";
      // SW-WV-F6: narrow the Rust-stamped renderTarget to the three valid values;
      // anything else (or absent) → undefined (no bubble suffix).
      const rt = event.payload.renderTarget;
      const renderTarget: RenderTarget | undefined =
        rt === "terminal" || rt === "drill_doc" || rt === "artifact" ? rt : undefined;
      logger.debug("mcp", `agent-message received for session ${sessionId}`);
      useAgentChatStore.getState().appendMessage(sessionId, role, text, renderTarget);
    });
    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
}
