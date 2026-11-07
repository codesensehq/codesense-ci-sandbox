// SPDX-License-Identifier: MIT
import { useEffect, useEffectEvent, useRef, useState } from "react";
import { workspaceChatMarkViewed, workspaceChatRead } from "@/tauri/commands";
import { useDocumentVisible } from "@/features/inbox/hooks/use-document-visible";
import type { WorkspaceChatHandle } from "./use-workspace-chat";

/** One visible visit owns both its stable divider and human acknowledgement. */
export function useChatHumanView(
  workspaceId: string,
  chat: WorkspaceChatHandle,
  root: React.RefObject<HTMLElement | null>,
  visible = true,
): number | undefined {
  const shown = useDocumentVisible();
  const [baseline, setBaseline] = useState<{ workspace: string; id: number } | null>(null);
  const visit = useRef<{ workspace: string; ready: boolean; marked: number } | null>(null);
  const syncVisit = useEffectEvent(() => {
    const box = root.current?.getBoundingClientRect();
    if (!visible || !shown || !box || box.width <= 0 || box.height <= 0) {
      if (visit.current) {
        visit.current = null;
        setBaseline(null);
      }
      return;
    }
    if (chat.messages === null || chat.loading) return;
    if (!visit.current || visit.current.workspace !== workspaceId) {
      const current = { workspace: workspaceId, ready: false, marked: 0 };
      visit.current = current;
      // Read the authoritative position at each reveal: another surface may
      // have shown this room since our previous visit. This probe reads no posts.
      void workspaceChatRead(workspaceId, 0, 0)
        .then((position) => {
          if (visit.current !== current || !root.current) return;
          current.ready = true;
          current.marked = position.humanReadId ?? 0;
          setBaseline({ workspace: workspaceId, id: current.marked });
        })
        .catch(() => {
          if (visit.current === current) visit.current = null;
        });
      return;
    }
    const current = visit.current;
    const last = chat.messages.at(-1)?.messageId ?? 0;
    if (!current.ready || last <= current.marked) return;
    current.marked = last;
    void workspaceChatMarkViewed(workspaceId, last).catch(() => {
      if (visit.current === current) current.marked = 0;
    });
  });
  useEffect(() => {
    syncVisit();
    const element = root.current;
    if (!element) return;
    // Retained right-pane bodies reveal through size changes, not remounts.
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(syncVisit);
    observer?.observe(element);
    return () => observer?.disconnect();
  }, [workspaceId, chat.messages, chat.loading, visible, shown, root, baseline]);
  return baseline?.workspace === workspaceId ? baseline.id : undefined;
}
