// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useCallback, useLayoutEffect, useRef, useState } from "react";
import { toast } from "sonner";

interface UseFieldDraftOptions {
  /** The persisted value from the store. Used as the revert target. */
  committed: string;
  /** Async commit. Throws on invalid input; caller is responsible for the
   *  store call (e.g. `setMainDoc`). May return its canonical display value. */
  commit: (next: string) => Promise<void | string>;
  /** Label for the error toast on commit failure, e.g. "viewer root". */
  label: string;
  syncCommitted?: boolean;
}

/**
 * Draft + blur-commit + Enter/Escape semantics for async-validated text
 * inputs. Shared across Settings row types (Viewer root, Pipeline root,
 * workspace display name, etc. — any async-validated field).
 *
 * Contract:
 *  - Typing mutates a local draft; the store is untouched until blur.
 *  - Blur (or Enter) calls `commit(draft)`; on throw, the error is
 *    toasted, the draft is reverted to `committed`, and focus is returned
 *    to the input (AC-4: "revert … **without** blurring the user out").
 *  - Escape reverts the draft without committing.
 *  - A re-entry guard drops concurrent blurs; the dropped draft is
 *    toasted so the user is aware it was not persisted.
 *  - A synchronous ref mirror of `draft` keeps the blur handler correct
 *    across Escape's `setDraft` → `blur()` race.
 */
export function useFieldDraft({
  committed,
  commit,
  label,
  syncCommitted = false,
}: UseFieldDraftOptions) {
  const [draft, setDraft] = useState(committed);
  const draftRef = useRef(draft);
  const commitInFlightRef = useRef(false);
  // State mirror of the in-flight ref so callers can disable the input
  // while an async commit is pending (e.g., the workspace rename PR
  // flow takes seconds — a bare ref wouldn't trigger a re-render).
  const [isCommitting, setIsCommitting] = useState(false);

  const updateDraft = useCallback((next: string) => {
    setDraft(next);
    draftRef.current = next;
  }, []);

  const dirty = useRef(false);
  const latestCommitted = useRef(committed);
  useLayoutEffect(() => {
    latestCommitted.current = committed;
    if (draftRef.current === committed) dirty.current = false;
    if (syncCommitted && !dirty.current && !commitInFlightRef.current) updateDraft(committed);
  }, [committed, syncCommitted, updateDraft]);

  const runCommit = useCallback(
    async (input: HTMLInputElement | null) => {
      const current = draftRef.current;
      const before = latestCommitted.current;
      if (commitInFlightRef.current) {
        toast.info(`${label} change is still saving — retry after it completes.`);
        return;
      }
      if (current === committed) {
        dirty.current = false;
        return;
      }
      commitInFlightRef.current = true;
      setIsCommitting(true);
      try {
        const saved = await commit(current);
        if (syncCommitted && draftRef.current === current) {
          dirty.current = false;
          if (latestCommitted.current !== before) updateDraft(latestCommitted.current);
          else if (typeof saved === "string") updateDraft(saved);
          else if (current.trim() === latestCommitted.current) updateDraft(latestCommitted.current);
        }
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        toast.error(`Invalid ${label}: ${msg}`);
        dirty.current = false;
        updateDraft(syncCommitted ? latestCommitted.current : committed);
        // Keep the user inside the field so they can correct the
        // value without having to click back in.
        input?.focus();
      } finally {
        commitInFlightRef.current = false;
        setIsCommitting(false);
      }
    },
    [commit, committed, label, updateDraft, syncCommitted],
  );

  const onBlur = useCallback(
    (e: React.FocusEvent<HTMLInputElement>) => {
      const field = e.currentTarget;
      commitTextEditOnBlur(field, () => {
        void runCommit(field);
      });
    },
    [runCommit],
  );

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        e.currentTarget.blur();
      } else if (e.key === "Escape") {
        dirty.current = false;
        updateDraft(committed);
        e.currentTarget.blur();
      }
    },
    [committed, updateDraft],
  );

  const onChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      dirty.current = e.currentTarget.value !== latestCommitted.current;
      updateDraft(e.currentTarget.value);
    },
    [updateDraft],
  );

  return { draft, onChange, onBlur, onKeyDown, isCommitting };
}
