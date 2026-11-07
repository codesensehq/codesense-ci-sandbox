// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useState, useCallback, useRef } from "react";
import { X, Plus } from "lucide-react";

import { validateListChipValue } from "../lib/metadata-schema";
import { InvalidFlagChip, useInvalidFlag } from "./metadata-field-controls";

const MAX_VISIBLE_CHIPS = 6;

interface MetadataListChipEditorProps {
  /** Current list items. */
  items: string[];
  /** Fires with the full new item list after an add/remove. */
  onChange: (items: string[]) => void;
  disabled?: boolean;
  /** Accessible label naming the field (canonical name). */
  ariaLabel: string;
  /** SW-AU-77I: `true` when a `multi_select` tag has an out-of-vocab item. The flag
   *  is ROW-LEVEL (the reason names the first offending item); per-chip amber is out
   *  of scope. Defaults off, so the known list fields are unchanged. */
  invalid?: boolean;
  /** Short flag label shown when `invalid`. */
  invalidFlag?: string;
  /** Full invalid reason — announced via the group's `aria-describedby`. */
  invalidReason?: string;
}

/**
 * SLW-MD-1 (Task 6.3): reusable list-chip editor for `depends_on`,
 * `supporting_docs`, `repos`, `scope_context`, `scope_install`. Chips are
 * removable; a type-to-filter "+ add" input appends. Collapses to "+N more"
 * past ~6 chips. Rejects a value containing a comma with inline feedback
 * (AC-3 parity — HTML CSV-joins lists, so a comma would split on round-trip).
 */
export function MetadataListChipEditor({
  items,
  onChange,
  disabled,
  ariaLabel,
  invalid = false,
  invalidFlag,
  invalidReason,
}: MetadataListChipEditorProps) {
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const { showFlag, reasonId, describedBy } = useInvalidFlag(
    invalid,
    items.length > 0,
    invalidReason,
  );

  const commitAdd = useCallback(
    (text = draft) => {
      const err = validateListChipValue(text);
      if (err) {
        setError(err);
        return;
      }
      const value = text.trim();
      if (items.includes(value)) {
        // Idempotent: don't duplicate, just clear the draft.
        setDraft("");
        setError(null);
        setAdding(false);
        return;
      }
      onChange([...items, value]);
      setDraft("");
      setError(null);
      setAdding(false);
    },
    [draft, items, onChange],
  );

  const remove = useCallback(
    (value: string) => {
      onChange(items.filter((i) => i !== value));
    },
    [items, onChange],
  );

  const visible = expanded ? items : items.slice(0, MAX_VISIBLE_CHIPS);
  const overflow = items.length - visible.length;

  return (
    <div className="flex flex-col gap-1">
      <div
        className="flex flex-wrap items-center gap-1.5"
        aria-label={ariaLabel}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy}
        role="group"
      >
        {visible.map((item) => (
          <span
            key={item}
            className="inline-flex items-center gap-1 rounded border py-0.5 pr-1 pl-1.5 text-[11.5px]"
            style={{
              backgroundColor: "var(--popover, var(--card))",
              borderColor: "var(--border)",
              color: "var(--foreground)",
            }}
          >
            <span className="max-w-[220px] truncate">{item}</span>
            {!disabled && (
              <button
                type="button"
                aria-label={`Remove ${item}`}
                className="inline-flex rounded-sm p-0.5 hover:text-[var(--danger,#dc2626)]"
                style={{ color: "var(--muted-foreground)" }}
                onClick={() => remove(item)}
              >
                <X size={11} />
              </button>
            )}
          </span>
        ))}

        {overflow > 0 && (
          <button
            type="button"
            className="px-1 py-0.5 text-[11px]"
            style={{ color: "var(--muted-foreground)" }}
            onClick={() => setExpanded(true)}
          >
            +{overflow} more
          </button>
        )}

        {!disabled &&
          (adding ? (
            <input
              ref={inputRef}
              autoFocus
              value={draft}
              aria-label={`Add ${ariaLabel}`}
              className="min-w-[120px] rounded border px-1.5 py-0.5 text-[11.5px] outline-none focus-visible:ring-2 focus-visible:ring-[var(--ring)]"
              style={{
                backgroundColor: "var(--background)",
                borderColor: error ? "var(--amber-border, var(--border))" : "var(--border)",
                color: "var(--foreground)",
              }}
              onChange={(e) => {
                setDraft(e.target.value);
                if (error) setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commitAdd();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  setAdding(false);
                  setDraft("");
                  setError(null);
                }
              }}
              onBlur={(event) => {
                const field = event.currentTarget;
                commitTextEditOnBlur(field, () => {
                  if (field.value.trim() === "") {
                    setAdding(false);
                    setError(null);
                  } else commitAdd(field.value);
                });
              }}
            />
          ) : (
            <button
              type="button"
              className="inline-flex items-center gap-1 rounded border border-dashed px-2 py-0.5 text-[11.5px]"
              style={{ borderColor: "var(--border)", color: "var(--muted-foreground)" }}
              onClick={() => {
                setAdding(true);
                setDraft("");
                setError(null);
              }}
            >
              <Plus size={11} />
              add
            </button>
          ))}

        {showFlag && (
          <InvalidFlagChip label={invalidFlag} reason={invalidReason} reasonId={reasonId} />
        )}
      </div>

      {error && (
        <span className="text-[10.5px]" style={{ color: "var(--amber, #d97706)" }}>
          {error}
        </span>
      )}
    </div>
  );
}
