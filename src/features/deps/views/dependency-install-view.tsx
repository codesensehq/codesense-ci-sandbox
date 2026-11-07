// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { AlertCircle, Download, RotateCw } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { InlineAlert } from "@/components/ui/inline-alert";
import { AppButton } from "@/shared/components/app-button";
import { BrandMark } from "@/shared/components/brand-mark";
import { Spinner } from "@/shared/components/spinner";
import {
  installCommandLineTools,
  recheckDependencies,
  type DependencyStatus,
} from "@/tauri/commands";

const GIT_DOWNLOAD_URL = "https://git-scm.com/downloads";

// codesense uses the user's own system `git` — it no longer downloads or pins a
// managed git binary (that logic and its GitHub-release dependency were removed).
// Git is detected on startup (known locations + login-shell PATH probe,
// version-gated). This screen is a GUIDANCE gate: it appears only when a user has
// no git (or one below the floor) on PATH, and it offers a re-check rather than an
// install. macOS/Linux ship git, so the common path never sees this screen.
//
// SW-AN-3: Claude is optional (ADR-3) and never gates the app. codesense's own MCP
// bridge is not a boot gate either (SW-CP-25/CP-30 auto-provision it).

// Minimum git version codesense requires (kept in sync with MIN_GIT_VERSION in deps.rs).
const MIN_GIT_VERSION = "2.44";

interface DependencyInstallViewProps {
  initialStatus: DependencyStatus;
  onComplete: () => void;
}

/**
 * Git guidance gate. If git is already available (e.g. the status prop is stale
 * by the time we mount), unblock immediately. Otherwise show install guidance and
 * a Re-check button that re-runs detection off the main thread.
 */
export function DependencyInstallView({ initialStatus, onComplete }: DependencyInstallViewProps) {
  const [status, setStatus] = useState<DependencyStatus>(initialStatus);
  const [rechecking, setRechecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [installNote, setInstallNote] = useState<string | null>(null);
  // What failed and the thrown value behind it (shown through the user-facing path).
  const [error, setError] = useState<{ title: string; reason: unknown } | null>(null);

  // If git is (or becomes) available, unblock the app.
  useEffect(() => {
    if (status.git.available) onComplete();
  }, [status.git.available, onComplete]);

  // Trigger Apple's Command Line Tools installer (provides git). It launches
  // Apple's GUI installer and returns immediately — the user completes the dialog,
  // then hits Re-check. Not a silent install (Apple gates it behind its own dialog).
  async function onInstall() {
    if (installing) return;
    setInstalling(true);
    setError(null);
    setInstallNote(null);
    try {
      const res = await installCommandLineTools();
      if (res.status === "triggered") {
        setInstallNote("Apple's installer is opening. Finish it, then click Re-check.");
      } else if (res.status === "already_installed") {
        setInstallNote(
          "Command Line Tools are already installed. Click Re-check — if git is still too old, install a newer one with Homebrew.",
        );
      } else {
        setError({ title: "Couldn't start the installer", reason: res.message ?? null });
      }
    } catch (e) {
      setError({ title: "Couldn't start the installer", reason: e });
    } finally {
      setInstalling(false);
    }
  }

  async function onRecheck() {
    // Guard synchronously — the `disabled` prop lags the click by a render, so a
    // fast double-click would otherwise fire two concurrent detection probes.
    if (rechecking) return;
    setRechecking(true);
    setError(null);
    try {
      // Only update status — the `status.git.available` effect is the single
      // completion trigger, so `setStatus` flipping it true fires onComplete once.
      // (Calling onComplete() directly here too would double-fire it.)
      setStatus(await recheckDependencies());
    } catch (e) {
      setError({ title: "Couldn't check for Git", reason: e });
    } finally {
      setRechecking(false);
    }
  }

  // Detected but gated out (a version was parsed yet git isn't available) → too old.
  // Guard on `!available` so a resolved git never trips the "too old" copy during
  // the brief window before the onComplete effect unblocks the app.
  const belowFloor = !status.git.available && status.git.version != null;

  return (
    <div className="flex-1 min-h-0 overflow-x-hidden overflow-y-auto flex flex-col items-center">
      <div className="w-full max-w-sm px-4 my-auto">
        {/* Logo + heading */}
        <div className="text-center mb-6">
          <BrandMark />
          <h1 className="text-lg font-semibold">Git is required</h1>
          <p className="text-sm text-muted-foreground mt-1">
            {belowFloor
              ? `CodeSense needs Git ${MIN_GIT_VERSION} or newer. Found v${status.git.version}.`
              : `CodeSense couldn't find Git ${MIN_GIT_VERSION}+ on your PATH.`}
          </p>
        </div>

        {/* Guidance */}
        <div className="border rounded-lg px-4 py-3 text-sm text-muted-foreground space-y-2">
          <div className="flex items-start gap-2">
            <AlertCircle size={16} className="mt-0.5 shrink-0 text-destructive" />
            <div className="space-y-2">
              <p>Install Git, then re-check. The quickest way:</p>
              <ul className="list-disc pl-4 space-y-1">
                <li>
                  Click <span className="font-medium text-foreground">Install Git</span> below — it
                  opens Apple's Command Line Tools installer.
                </li>
                <li>
                  Prefer a newer git? <code className="text-xs">brew install git</code>
                </li>
                <li>
                  Or download from{" "}
                  <button
                    type="button"
                    className="underline hover:text-foreground"
                    onClick={() => openUrl(GIT_DOWNLOAD_URL).catch(() => {})}
                  >
                    git-scm.com/downloads
                  </button>
                </li>
              </ul>
            </div>
          </div>
        </div>

        {installNote && <p className="text-xs text-muted-foreground mt-3">{installNote}</p>}
        {error && (
          <InlineAlert severity="error" className="mt-3" title={error.title} error={error.reason} />
        )}

        {/* Actions: install (primary) + re-check (secondary) */}
        <AppButton
          type="button"
          size="lg"
          className="mt-4 w-full"
          onClick={onInstall}
          disabled={installing}
        >
          {installing ? (
            <>
              <Spinner decorative />
              Opening installer…
            </>
          ) : (
            <>
              <Download aria-hidden />
              Install Git
            </>
          )}
        </AppButton>
        <AppButton
          type="button"
          size="lg"
          variant="outline"
          className="mt-2 w-full"
          onClick={onRecheck}
          disabled={rechecking}
        >
          {rechecking ? (
            <>
              <Spinner decorative />
              Checking…
            </>
          ) : (
            <>
              <RotateCw aria-hidden />
              Re-check
            </>
          )}
        </AppButton>
      </div>
    </div>
  );
}
