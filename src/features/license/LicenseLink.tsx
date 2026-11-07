// SPDX-License-Identifier: MIT
import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { COPY } from "./copy";

/** Keeps the destination available when the system browser cannot open it. */
export function LicenseLink({
  href,
  label,
  testId,
}: {
  href: string;
  label: string;
  testId?: string;
}) {
  const [failed, setFailed] = useState(false);
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <span>
      <button
        type="button"
        className="text-left underline underline-offset-2 hover:text-foreground"
        data-testid={testId}
        onClick={async () => {
          try {
            await openUrl(href);
            setFailed(false);
          } catch {
            setFailed(true);
          }
        }}
      >
        {label}
      </button>
      {failed && (
        <span className="mt-2 block text-sm" role="status">
          {COPY.browserFailed} <span className="select-text break-all">{href}</span>{" "}
          <button
            type="button"
            className="underline underline-offset-2"
            onClick={async () => {
              try {
                await navigator.clipboard.writeText(href);
                setCopy("copied");
              } catch {
                setCopy("failed");
              }
            }}
          >
            {copy === "copied" ? COPY.copiedUrl : COPY.copyUrl}
          </button>
          {copy === "failed" && <span> {COPY.copyFailed}</span>}
        </span>
      )}
    </span>
  );
}
