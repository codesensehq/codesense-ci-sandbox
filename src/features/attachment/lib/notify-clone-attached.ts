// SPDX-License-Identifier: MIT
import { toast } from "sonner";

/**
 * SW-OB-3 (AC-2): the single confirmation toast for a newly-attached local
 * clone. Both entry points share THIS helper so the wording can't diverge:
 *  - the silent-record path (`use-source-repo-attachment.ts`) — fired only on
 *    a brand-new insert, never on a `remote_url`-changed update or re-attach.
 *  - the explicit `AttachLocalCloneDialog` success path.
 */
export function notifyLocalCloneAttached(params: { owner: string; repo: string }): void {
  toast.success(`Attached your local checkout of ${params.owner}/${params.repo}.`);
}
