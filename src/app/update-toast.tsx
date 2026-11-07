// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { persistent } from "@/components/ui/persistent-toast";
import {
  checkForUpdate,
  downloadAndInstallUpdate,
  type UpdateInfo,
  type UpdateError,
} from "@/tauri/commands";
import { X } from "lucide-react";
import { openAppSettings } from "@/features/settings/lib/open-settings";
import { isBetaChannel, readBuildChannel, type BuildChannelInfo } from "@/features/license/channel";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const TOAST_ID = "update";
const SIMULATE = false;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatBytes(bytes: number): string {
  const mb = bytes / (1024 * 1024);
  return `${mb.toFixed(1)} MB`;
}

function baseErrorMessage(err: UpdateError | string): string {
  if (typeof err === "string") return err;
  switch (err.kind) {
    case "auth":
      return "Could not access the update. Try signing out and back in.";
    case "network":
      return "Could not download the update. Check your connection and try again.";
    case "signature":
      return "Update verification failed. The download may be corrupted. Try again.";
    case "notFound":
      return "Update not found. It may not be available for your platform yet.";
    default:
      return "Something went wrong. Try again.";
  }
}

/** Where a beta build sends someone for fresh download links (SW-PB-13 AC-6). */
export function betaResendUrl(info: BuildChannelInfo): string {
  return `https://${info.publicHost}/beta/resend`;
}

/**
 * The update error as shown. Internal builds show the error only. A beta build also says
 * where to get fresh download links, so an error is never a dead end.
 */
export function errorMessage(
  err: UpdateError | string,
  info: BuildChannelInfo = readBuildChannel(),
): string {
  const message = baseErrorMessage(err);
  if (!isBetaChannel(info)) return message;
  return `${message} To get the newest build now, ask for fresh download links at ${betaResendUrl(info)}.`;
}

function isAuthError(err: UpdateError | string): boolean {
  return typeof err !== "string" && err.kind === "auth";
}

// Exported so the rewire is independently testable. Re-auth flow lands
// on the Account section where sign-in lives. Pass the clicked toast
// button as trigger when available — the toast button lives in a portal
// outside the normal focus chain, so `document.activeElement` isn't
// reliable here.
export function navigateToSettings(trigger?: HTMLElement | null) {
  openAppSettings("account", trigger);
}

// ---------------------------------------------------------------------------
// Module-level store — lives outside React so Sonner toast re-renders work.
// Uses useSyncExternalStore pattern for proper reactivity.
// ---------------------------------------------------------------------------

type UpdatePhase =
  | { type: "available" }
  | { type: "downloading" }
  | { type: "installing" }
  | { type: "success" }
  | { type: "error"; error: UpdateError | string };

interface UpdateState {
  phase: UpdatePhase;
  info: UpdateInfo | null;
  bytesDownloaded: number;
  totalBytes: number | null;
}

let state: UpdateState = {
  phase: { type: "available" },
  info: null,
  bytesDownloaded: 0,
  totalBytes: null,
};

const listeners = new Set<() => void>();

function getSnapshot(): UpdateState {
  return state;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function setState(partial: Partial<UpdateState>) {
  state = { ...state, ...partial };
  listeners.forEach((l) => l());
}

function useUpdateState(): UpdateState {
  return useSyncExternalStore(subscribe, getSnapshot);
}

// ---------------------------------------------------------------------------
// Tauri event listener — set up once, writes to module store
// ---------------------------------------------------------------------------

let eventListenerActive = false;

function ensureEventListener() {
  if (eventListenerActive) return;
  eventListenerActive = true;

  listen<{ bytesDownloaded: number; totalBytes: number | null }>(
    "update-download-progress",
    (event) => {
      setState({
        bytesDownloaded: event.payload.bytesDownloaded,
        totalBytes: event.payload.totalBytes,
      });
    },
  );
}

// ---------------------------------------------------------------------------
// Download guard — prevents concurrent startDownload calls
// ---------------------------------------------------------------------------

let downloadInProgress = false;

// ---------------------------------------------------------------------------
// Toast component — reads from module store, always re-renders on changes
// ---------------------------------------------------------------------------

function UpdateToast() {
  const { phase, info, bytesDownloaded, totalBytes } = useUpdateState();

  if (!info) return null;

  const pct = totalBytes ? Math.round((bytesDownloaded / totalBytes) * 100) : null;

  return (
    <div
      data-cs-toast-body
      className="flex flex-col gap-2 w-[356px] p-4 rounded-lg border shadow-lg"
      style={{
        backgroundColor: "var(--popover)",
        color: "var(--popover-foreground)",
        borderColor: "var(--border)",
      }}
    >
      {phase.type === "available" && (
        <>
          <p className="text-sm font-semibold">Update v{info.version} available</p>
          <p className="text-sm text-muted-foreground">v{info.version} is ready to install.</p>
          <div className="flex gap-2 mt-1">
            <button
              className="px-3 py-1.5 text-xs font-medium rounded-md border bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
              onClick={() => startDownload()}
            >
              Install &amp; Restart
            </button>
            <button
              className="px-3 py-1.5 text-xs font-medium rounded-md border border-border text-muted-foreground hover:bg-accent transition-colors"
              onClick={() => toast.dismiss(TOAST_ID)}
            >
              Later
            </button>
          </div>
        </>
      )}

      {phase.type === "downloading" && (
        <>
          <p className="text-sm font-semibold">Downloading v{info.version}...</p>
          <div
            className="w-full rounded-sm overflow-hidden"
            style={{
              height: 6,
              backgroundColor: "var(--muted)",
            }}
          >
            <div
              className="h-full rounded-sm"
              style={{
                width: `${pct ?? 0}%`,
                backgroundColor: "var(--primary)",
                transition: "width 300ms ease",
              }}
            />
          </div>
          <p className="text-xs text-muted-foreground">
            {pct != null ? (
              <>
                {pct}% &middot; {formatBytes(bytesDownloaded)} / {formatBytes(totalBytes!)}
              </>
            ) : bytesDownloaded > 0 ? (
              formatBytes(bytesDownloaded)
            ) : (
              "Starting download..."
            )}
          </p>
        </>
      )}

      {phase.type === "installing" && (
        <>
          <p className="text-sm font-semibold">Installing v{info.version}...</p>
          <div
            className="w-full rounded-sm overflow-hidden"
            style={{
              height: 6,
              backgroundColor: "var(--muted)",
            }}
          >
            <div
              className="h-full rounded-sm"
              style={{
                width: "100%",
                backgroundColor: "var(--primary)",
              }}
            />
          </div>
          <p className="text-xs text-muted-foreground">The app will restart momentarily.</p>
        </>
      )}

      {phase.type === "success" && (
        <p className="text-sm font-semibold">✓ Updated to v{info.version} — restarting...</p>
      )}

      {phase.type === "error" &&
        (() => {
          const showRetry = typeof phase.error === "string" || phase.error.kind !== "notFound";
          return (
            <>
              <div className="flex items-start justify-between">
                <p className="text-sm font-semibold text-destructive">Update failed</p>
                <button
                  className="text-muted-foreground hover:text-foreground transition-colors -mt-1 -mr-1 p-1"
                  onClick={() => toast.dismiss(TOAST_ID)}
                  aria-label="Dismiss"
                >
                  <X className="size-3.5" />
                </button>
              </div>
              <p className="text-sm text-foreground/80">{errorMessage(phase.error)}</p>
              <div className="flex gap-2 mt-1">
                {showRetry && (
                  <button
                    className="px-3 py-1.5 text-xs font-medium rounded-md border bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
                    onClick={() => startDownload()}
                  >
                    Retry
                  </button>
                )}
              </div>
            </>
          );
        })()}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

let scopePromptShownThisSession = false;

export async function runUpdateCheck() {
  // The local-only webdriver bundle must never contact the production updater.
  if (import.meta.env.MODE === "webdriver") return;
  try {
    const info = await checkForUpdate(SIMULATE || undefined);

    if (!info) return;

    ensureEventListener();
    // Reset all state for a fresh update flow
    setState({
      info,
      phase: { type: "available" },
      bytesDownloaded: 0,
      totalBytes: null,
    });

    toast.custom(
      () => <UpdateToast />,
      persistent({
        id: TOAST_ID,
        dismissible: false,
      }),
    );
  } catch (e) {
    const err = e as UpdateError | string;
    console.error("[update] check failed:", err);

    // Show re-auth prompt with settings navigation
    if (isAuthError(err) && !scopePromptShownThisSession) {
      scopePromptShownThisSession = true;
      toast(
        "Auto-updates need additional access",
        persistent({
          id: "update-scope",
          description: "Sign out and back in to enable automatic updates.",
          action: {
            label: "Go to Settings",
            onClick: (event) => navigateToSettings(event.currentTarget as HTMLElement),
          },
        }),
      );
    }
  }
}

// ---------------------------------------------------------------------------
// Download flow
// ---------------------------------------------------------------------------

async function startDownload() {
  // Guard against concurrent calls
  if (downloadInProgress) return;
  downloadInProgress = true;

  // Reset progress for this download attempt
  setState({
    phase: { type: "downloading" },
    bytesDownloaded: 0,
    totalBytes: null,
  });

  try {
    await downloadAndInstallUpdate(SIMULATE || undefined);

    setState({ phase: { type: "installing" } });

    await new Promise((r) => setTimeout(r, 1500));

    setState({ phase: { type: "success" } });

    if (!SIMULATE) {
      await new Promise((r) => setTimeout(r, 2000));
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    }
  } catch (e) {
    const err = e as UpdateError | string;
    console.error("[update] install failed:", err);
    setState({ phase: { type: "error", error: err } });
  } finally {
    downloadInProgress = false;
  }
}
