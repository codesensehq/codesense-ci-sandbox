// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useState } from "react";

import { ExternalLink } from "lucide-react";

import { HtmlViewer } from "@/features/dashboard/components/html-viewer";
import {
  messageDocKey,
  INITIAL_MESSAGE_HEIGHT,
  messageFrameHeight,
} from "../lib/inbox-message-frame";
import { cn } from "@/lib/utils";
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { listPanes, type InboxTeam } from "@/tauri/commands";

/**
 * An `html` inbox message to the human, rendered as a live dashboard: the same
 * `HtmlViewer` frame, `window.codesense` bridge, pushed events and action gate a
 * dashboard file gets. The frame is identified by the message (team, id, sender),
 * so team-scoped bridge calls act in the message's own team and the gate keys on
 * the message, not on any file.
 *
 * Inline it sizes to the content the doc reports; "Open in tab" shows the same
 * message full-size as a viewer tab.
 */
export interface InboxMessageDashboardProps {
  team: InboxTeam;
  messageId: number;
  sender: string;
  /** `inline` sits in a conversation, sized to its content; `tab` fills its host. */
  variant?: "inline" | "tab";
  className?: string;
}

// The team content root each message team's bridge reads anchor in, resolved once
// per team per app session. A failed lookup is not cached, so the next frame retries.
const contentRoots = new Map<string, Promise<string | null>>();
function teamContentRoot(team: InboxTeam): Promise<string | null> {
  const key = `${team.org}:${team.team}`;
  let pending = contentRoots.get(key);
  if (!pending) {
    pending = listPanes(team.org, team.team).then(
      (listing) => listing.contentRoot || null,
      () => {
        contentRoots.delete(key);
        return null;
      },
    );
    contentRoots.set(key, pending);
  }
  return pending;
}

/** Test-only: forget resolved content roots. */
export function __resetMessageContentRootsForTest(): void {
  contentRoots.clear();
}

/** `undefined` while resolving; a root, or `""` when the team has none on this machine. */
function useTeamContentRoot(team: InboxTeam): string | undefined {
  const [resolved, setResolved] = useState<{ key: string; root: string } | null>(null);
  const key = `${team.org}:${team.team}`;
  useEffect(() => {
    let cancelled = false;
    void teamContentRoot({ org: team.org, team: team.team }).then((root) => {
      if (!cancelled) setResolved({ key, root: root ?? "" });
    });
    return () => {
      cancelled = true;
    };
  }, [key, team.org, team.team]);
  return resolved?.key === key ? resolved.root : undefined;
}

export function InboxMessageDashboard({
  team,
  messageId,
  sender,
  variant = "inline",
  className,
}: InboxMessageDashboardProps): React.JSX.Element {
  const contentRoot = useTeamContentRoot(team);
  const [height, setHeight] = useState<number | null>(null);
  const onContentSize = useCallback((h: number) => {
    setHeight(messageFrameHeight(h));
  }, []);
  const inline = variant === "inline";
  const message = { org: team.org, team: team.team, messageId, sender };
  return (
    <div
      data-testid="inbox-message-body"
      data-content-type="html"
      data-live="true"
      className={cn("flex w-full flex-col", inline ? "gap-1" : "h-full", className)}
    >
      <div
        data-testid="inbox-message-frame"
        className={cn("w-full overflow-hidden", inline ? "rounded border border-border" : "flex-1")}
        style={inline ? { height: height ?? INITIAL_MESSAGE_HEIGHT } : undefined}
      >
        {contentRoot === undefined ? (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            Loading…
          </div>
        ) : (
          <HtmlViewer
            filePath={messageDocKey(message)}
            teamContentPath={contentRoot}
            message={message}
            onContentSize={inline ? onContentSize : undefined}
            renderMissing={() => (
              <p className="p-2 text-xs text-muted-foreground">
                This message is no longer available.
              </p>
            )}
          />
        )}
      </div>
      {inline && (
        <button
          type="button"
          data-testid="inbox-message-open-tab"
          onClick={() => openMessageTab(team, messageId, sender)}
          className="inline-flex items-center gap-1 self-end text-[0.65rem] text-muted-foreground hover:text-foreground"
        >
          <ExternalLink className="size-3" aria-hidden />
          Open in tab
        </button>
      )}
    </div>
  );
}

// ── Open in tab ────────────────────────────────────────────────────────────

/** Virtual tab prefix: `virtual://inbox-message/<org>/<team>/<id>/<sender>`. */
export const MESSAGE_TAB_PREFIX = "virtual://inbox-message";

export function messageTabPath(team: InboxTeam, messageId: number, sender: string): string {
  return [MESSAGE_TAB_PREFIX, team.org, team.team, String(messageId), sender]
    .map((part, i) => (i === 0 ? part : encodeURIComponent(part)))
    .join("/");
}

/** The message a tab path names, or `null` for a malformed path. */
export function parseMessageTabPath(
  path: string,
): { team: InboxTeam; messageId: number; sender: string } | null {
  if (!path.startsWith(`${MESSAGE_TAB_PREFIX}/`)) return null;
  const parts = path.slice(MESSAGE_TAB_PREFIX.length + 1).split("/");
  if (parts.length !== 4) return null;
  try {
    const [org, teamName, id, sender] = parts.map(decodeURIComponent);
    const messageId = Number(id);
    if (!org || !teamName || !Number.isSafeInteger(messageId)) return null;
    return { team: { org, team: teamName }, messageId, sender };
  } catch {
    return null;
  }
}

function MessageTab({ filePath }: { filePath: string }): React.JSX.Element {
  const parsed = parseMessageTabPath(filePath);
  if (!parsed) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        This message link is not valid.
      </div>
    );
  }
  return (
    <InboxMessageDashboard
      team={parsed.team}
      messageId={parsed.messageId}
      sender={parsed.sender}
      variant="tab"
    />
  );
}

let registered = false;

/** Register the message tab under its virtual prefix. Idempotent. */
export function registerMessageTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(
    MESSAGE_TAB_PREFIX,
    (path) => {
      const parsed = parseMessageTabPath(path);
      return parsed ? `Message from ${parsed.sender}` : "Message";
    },
    MessageTab,
  );
}

/** Open (or focus) an html message as a full-size tab. */
export function openMessageTab(
  team: InboxTeam,
  messageId: number,
  sender: string,
  opts?: { keepLayout?: boolean },
): void {
  registerMessageTab();
  openManagementTab(messageTabPath(team, messageId, sender), opts);
}
