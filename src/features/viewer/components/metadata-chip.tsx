// SPDX-License-Identifier: MIT
import { forwardRef } from "react";
import { Tags } from "lucide-react";

import { cn } from "@/lib/utils";
import { HEADER_BTN } from "./header-button";

interface MetadataChipProps {
  open: boolean;
  onToggle: () => void;
  /** Invalid metadata mirrors the navigation's amber badge. */
  metadataValid?: boolean;
  regionId: string;
  className?: string;
}

export const MetadataChip = forwardRef<HTMLButtonElement, MetadataChipProps>(function MetadataChip(
  { open, onToggle, metadataValid, regionId, className },
  ref,
) {
  const invalid = metadataValid === false;
  return (
    <button
      ref={ref}
      type="button"
      aria-label={invalid ? "Document metadata, invalid" : "Document metadata"}
      title={invalid ? "Metadata · invalid" : "Metadata"}
      aria-expanded={open}
      aria-controls={regionId}
      className={cn(HEADER_BTN, className)}
      onClick={(e) => {
        e.stopPropagation();
        onToggle();
      }}
    >
      <Tags size={12} aria-hidden="true" />
      {invalid && (
        <span
          className="inline-block size-1.5 shrink-0 rounded-full bg-[var(--ibx-amber)]"
          aria-hidden="true"
        />
      )}
      <span className="viewer-header-label leading-none">Metadata</span>
    </button>
  );
});
