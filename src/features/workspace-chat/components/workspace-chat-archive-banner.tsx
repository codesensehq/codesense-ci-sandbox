// SPDX-License-Identifier: MIT
export interface WorkspaceChatArchiveBannerProps {
  displayName: string;
  onUnarchive: () => void | Promise<void>;
  onExport: () => void | Promise<void>;
}

/**
 * SW-WC-3 — the archive banner (variant C) that replaces the compose when a
 * workspace is archived.
 *
 * Three affordances:
 * - **Unarchive workspace** — calls SW-WC-1's `unarchive_workspace`.
 * - **Export transcript** — writes a markdown render to the content tree.
 * - **Find in chat** — v2 placeholder. The button is present (per the mock)
 *   but non-functional in v1; a follow-up story will wire the inline search.
 */
export function WorkspaceChatArchiveBanner({
  displayName,
  onUnarchive,
  onExport,
}: WorkspaceChatArchiveBannerProps): React.JSX.Element {
  return (
    <div
      className="wc-archived"
      role="status"
      aria-live="polite"
      data-testid="workspace-chat-archive-banner"
    >
      <span className="wc-archived__icon" aria-hidden="true">
        <ArchiveIcon />
      </span>
      <div>
        <div className="wc-archived__title">
          <h3>Chat archived — read-only</h3>
          <span className="wc-archived__badge">locked</span>
        </div>
        <div className="wc-archived__body">
          <span>
            Archived when <b>{displayName}</b> closed. External sessions can no longer post; history
            is preserved and searchable.
          </span>
        </div>
        <div className="wc-archived__actions">
          <button
            className="wc-archived__btn wc-archived__btn--primary"
            type="button"
            onClick={() => void onUnarchive()}
            data-testid="workspace-chat-unarchive"
          >
            <UnarchiveIcon />
            Unarchive workspace
          </button>
          <button
            className="wc-archived__btn"
            type="button"
            onClick={() => void onExport()}
            data-testid="workspace-chat-export"
          >
            <ExportIcon />
            Export transcript
          </button>
          <button
            className="wc-archived__btn"
            type="button"
            disabled
            title="Find in chat is coming in a follow-up"
            data-testid="workspace-chat-find"
          >
            <FindIcon />
            Find in chat
          </button>
        </div>
      </div>
    </div>
  );
}

function ArchiveIcon(): React.JSX.Element {
  return (
    <svg
      viewBox="0 0 20 20"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      aria-hidden="true"
    >
      <rect x="3" y="4" width="14" height="4" rx="1" />
      <path d="M4.5 8v6.5A1.5 1.5 0 0 0 6 16h8a1.5 1.5 0 0 0 1.5-1.5V8" />
      <path d="M8 11h4" />
    </svg>
  );
}
function UnarchiveIcon(): React.JSX.Element {
  return (
    <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <path d="M4 8h12M4 12h12M4 4v12" />
    </svg>
  );
}
function ExportIcon(): React.JSX.Element {
  return (
    <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <path d="M4 5h12M4 10h12M4 15h8" />
    </svg>
  );
}
function FindIcon(): React.JSX.Element {
  return (
    <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <circle cx="9" cy="9" r="5" />
      <path d="m13 13 3.5 3.5" />
    </svg>
  );
}
