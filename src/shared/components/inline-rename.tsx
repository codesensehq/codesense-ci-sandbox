// SPDX-License-Identifier: MIT
import { useEffect, useId, useRef, useState } from "react";
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";

interface InlineRenameProps {
  /** The current value, seeded into the field when editing begins. */
  value: string;
  /** Called with the trimmed next value on Enter or blur, only when it is
   *  non-empty AND actually changed. */
  onSubmit: (next: string) => void | Promise<string | null>;
  /** Called on Escape (cancel) — the parent hides the field. */
  onCancel: () => void;
  className?: string;
  ariaLabel?: string;
  validate?: (next: string) => string | null;
  commitOnBlur?: boolean;
}

/** SW-SN-2: the editing-state input shared by the note header (rename the active
 *  note) and a rail row's rename action. The PARENT owns when this renders (it
 *  toggles edit mode); this component owns only the field's keyboard contract:
 *  Enter/blur commits (if non-empty and changed), Escape cancels. Seeds + selects
 *  the text on mount so a rename starts ready to overwrite. */
export function InlineRename({
  value,
  onSubmit,
  onCancel,
  className,
  ariaLabel,
  validate,
  commitOnBlur = true,
}: InlineRenameProps) {
  const [draft, setDraft] = useState(value);
  const [error, setError] = useState<string | null>(null);
  const errorId = useId();
  const ref = useRef<HTMLInputElement>(null);
  // Guards against a blur firing a second commit after Enter/Escape already
  // resolved this edit (both paths null the parent's edit state, but the blur
  // still fires as the node unmounts).
  const done = useRef(false);

  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);

  const commit = async (text = draft) => {
    if (done.current) return;
    done.current = true;
    const next = text.trim();
    const problem = validate?.(next);
    if (problem) {
      setError(problem);
      done.current = false;
      return;
    }
    if (next && next !== value) {
      const result = await onSubmit(next);
      if (result) {
        setError(result);
        done.current = false;
      }
    } else onCancel();
  };

  const cancel = () => {
    if (done.current) return;
    done.current = true;
    onCancel();
  };

  return (
    <>
      <input
        ref={ref}
        className={className}
        aria-label={ariaLabel}
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? errorId : undefined}
        value={draft}
        onChange={(e) => {
          setDraft(e.target.value);
          setError(null);
        }}
        onBlur={
          commitOnBlur
            ? (event) => {
                const field = event.currentTarget;
                commitTextEditOnBlur(field, () => {
                  void commit(field.value);
                });
              }
            : undefined
        }
        onKeyDown={(e) => {
          // Keep the edit local — don't let Enter/Escape reach the panel's
          // ⌘B intercept or the global shortcut handler.
          e.stopPropagation();
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          } else if (e.key === "Tab" && !commitOnBlur) {
            e.preventDefault();
            cancel();
          } else if (e.key === "Escape") {
            e.preventDefault();
            cancel();
          }
        }}
      />
      {error && (
        <div
          id={errorId}
          aria-live="polite"
          className="max-w-64 px-2 py-1 text-xs text-destructive"
        >
          {error}
        </div>
      )}
    </>
  );
}
