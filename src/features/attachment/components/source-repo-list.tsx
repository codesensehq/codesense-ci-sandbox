// SPDX-License-Identifier: MIT
import { Fragment } from "react";
import { VirtualList } from "@/shared/components/virtual-list";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import type { Attachment, SourceRepoSlug } from "@/shared/types/domain";
import { SourceRepoPill } from "./source-repo-pill";

export interface SourceRepoListProps {
  slugs: SourceRepoSlug[];
  attachmentsByRemote: Map<string, Attachment>;
  orgSlug: string;
  teamName: string;
  /** Reconcile-owned unavailable copy keyed by canonical host/owner/repo. */
  unavailableBySlug?: ReadonlyMap<string, string>;
  /** SW-MC-2 (AC-7): the org's configured managed-clone root
   *  (`org.config_flags.workspace_root_dir`), or `null` when unset. Drives the
   *  managed-clone pill marker: `null` ⇒ hidden-default face (eye-slash), a set
   *  path ⇒ configured face (package). Never inferred from a path prefix. */
  workspaceRootDir?: string | null;
  /** Disable repository mutations while a required provider connection is missing. */
  actionsDisabled?: boolean;
  /** Test id for the list container. */
  "data-testid"?: string;
}

export function SourceRepoList({
  slugs,
  attachmentsByRemote,
  orgSlug,
  teamName,
  unavailableBySlug,
  "data-testid": dataTestId,
}: SourceRepoListProps) {
  const counts = new Map<string, number>();
  for (const slug of slugs) {
    const label = `${slug.owner}/${slug.repo}`.toLowerCase();
    counts.set(label, (counts.get(label) ?? 0) + 1);
  }
  const renderPill = (slug: SourceRepoSlug) => {
    const key = sourceRepoRemoteKey(slug);
    const label = `${slug.owner}/${slug.repo}`;
    return (
      <SourceRepoPill
        slug={slug}
        displayLabel={
          (counts.get(label.toLowerCase()) ?? 0) > 1 ? `${label} · ${key.split("/", 1)[0]}` : label
        }
        attachment={attachmentsByRemote.get(key) ?? null}
        orgSlug={orgSlug}
        teamName={teamName}
        unavailableLabel={unavailableBySlug?.get(key)}
      />
    );
  };
  if (slugs.length > 15)
    return (
      <VirtualList<SourceRepoSlug>
        items={slugs}
        estimateSize={32}
        getKey={sourceRepoRemoteKey}
        overscan={6}
        className="max-h-[200px] overflow-y-auto"
        data-testid={dataTestId}
        renderItem={renderPill}
      />
    );
  return (
    <div className="flex flex-wrap gap-1.5" data-testid={dataTestId}>
      {slugs.map((slug) => (
        <Fragment key={sourceRepoRemoteKey(slug)}>{renderPill(slug)}</Fragment>
      ))}
    </div>
  );
}
