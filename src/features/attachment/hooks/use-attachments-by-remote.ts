// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";

import type { Attachment } from "@/shared/types/domain";
import { useOrgStore } from "@/shared/stores/org-store";
import { listAttachmentsByRemote } from "@/tauri/commands";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { logger } from "@/shared/lib/logger";

/**
 * Load the current user's source-repo attachments keyed by remote (SW-HB-3,
 * extracted from `teams-card`). Version-gated on the org store's
 * `attachmentsVersion` so it fetches ONCE and re-fetches only when a
 * `bumpAttachmentsVersion()` fires (attach / detach / re-bind). Never blocks
 * first paint — the map starts empty and fills in when the fetch resolves.
 *
 * Both the Settings workspace-section and the Home Base source-repos panel use
 * this one loader; it is the single seam that feeds `SourceRepoPill` its
 * `Attachment` (which carries `github_account_id`, `remote_url`, `repo_path`).
 */
export function useAttachmentsByRemote(): Map<string, Attachment> {
  const attachmentsVersion = useOrgStore((s) => s.attachmentsVersion);
  const [attachmentsByRemote, setAttachmentsByRemote] = useState<Map<string, Attachment>>(
    () => new Map(),
  );

  const lastFetchedVersion = useRef(-1);
  useEffect(() => {
    if (lastFetchedVersion.current === attachmentsVersion) return;
    let stale = false;
    const currentUser = getCurrentUser();
    if (!currentUser) return;
    listAttachmentsByRemote(currentUser.github_user_id)
      .then((map) => {
        if (!stale) {
          setAttachmentsByRemote(map);
          lastFetchedVersion.current = attachmentsVersion;
        }
      })
      .catch((e) => {
        logger.warn("attachment", `listAttachmentsByRemote failed: ${e}`);
      });
    return () => {
      stale = true;
    };
  }, [attachmentsVersion]);

  return attachmentsByRemote;
}
