// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur, textMenuWithin } from "@/shared/lib/text-menu-focus";
import { useCallback, useEffect, useId, useRef, useState, useMemo } from "react";
import { ChevronDown, Search, GitBranch, Flag, Calendar } from "lucide-react";

import type { SourceRepoOption } from "@/tauri/commands";
import { Popover, PopoverAnchor, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { StageRail, StageRailShape } from "@/shared/components/stage-rail";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { MENU_STYLE, menuOptionClass, MenuCheck } from "@/shared/components/floating-menu";

// ---------------------------------------------------------------------------
// SLW-MD-1 (Task 6): per-field controls for the metadata band. All reuse the
// same CSS-var styling + portaled Radix `Popover` shell (SW-FX-79: the old
// `usePopoverDismiss` + `absolute top-full` shell is retired), parameterized for
// the metadata schema (the shared pickers are hard-wired to story/bug/spike +
// PipelineStatus, so they can't carry a free-text status combobox, a `(none)`
// type sentinel, or the invalid-pinned source_repo — hence these purpose-built
// controls).
// ---------------------------------------------------------------------------

const CTL_BASE =
  "flex w-full items-center gap-1.5 rounded-md border px-2 py-1 text-[12.5px] outline-none";
const MENU_CONTENT_CLASS =
  "max-h-60 min-w-[220px] overflow-y-auto rounded-md border p-1 shadow-lg gap-0 ring-0";
const popStyle = { ...MENU_STYLE, color: "var(--foreground)" } as const;

function ctlStyle(invalid?: boolean, disabled?: boolean) {
  return {
    backgroundColor: invalid ? "var(--amber-bg, var(--muted))" : "var(--popover, var(--card))",
    borderColor: invalid ? "var(--amber-border, var(--border))" : "var(--border)",
    color: "var(--foreground)",
    cursor: disabled ? "not-allowed" : "pointer",
    opacity: disabled ? 0.55 : 1,
  } as const;
}

const NONE_LABEL = "(none / plain doc)";

/**
 * The shared amber "⚑ <reason>" chip for a present-invalid field value — one home
 * for the invalid-flag look across every metadata control (source_repo, and the
 * SW-AU-77G/77I schema-declared rows), so the treatment can't drift between them.
 *
 * The short `label` is visible text (screen-reader legible via the control's name).
 * SW-AU-77I: the FULL `reason` is rendered into a visually-hidden node carrying
 * `reasonId`, which the control wires to its interactive element via
 * `aria-describedby` — so the reason is announced, not tooltip-only.
 */
export function InvalidFlagChip({
  label,
  reason,
  reasonId,
}: {
  label?: string;
  reason?: string;
  reasonId?: string;
}) {
  return (
    <span
      className="ml-auto inline-flex flex-shrink-0 items-center gap-1 text-[11px] font-semibold"
      style={{ color: "var(--amber, #d97706)" }}
    >
      <Flag size={11} />
      {label}
      {reason && reasonId ? (
        <span id={reasonId} className="sr-only">
          {reason}
        </span>
      ) : null}
    </span>
  );
}

/**
 * SW-AU-77I: the ONE place the present-invalid a11y wiring is derived, so the
 * option/text/date controls can't drift. Returns whether to show the flag, a stable
 * id for the reason node, and the `aria-describedby` value (set only when there is a
 * reason to announce). A control spreads `aria-describedby={describedBy}` on its
 * interactive element and passes `reason`/`reasonId` to {@link InvalidFlagChip}.
 */
export function useInvalidFlag(invalid: boolean, present: boolean, reason: string | undefined) {
  const reasonId = useId();
  const showFlag = invalid && present;
  const describedBy = showFlag && reason ? reasonId : undefined;
  return { showFlag, reasonId, describedBy };
}

// ---------------------------------------------------------------------------
// Generic option select (type, team) — constrained list + a "clear" sentinel.
// ---------------------------------------------------------------------------
interface OptionSelectProps {
  value: string | null;
  /** Constrained options. */
  options: readonly string[];
  onChange: (value: string | null) => void;
  disabled?: boolean;
  /** Placeholder shown when value is null. */
  noneLabel?: string;
  ariaLabel: string;
  /** SW-AU-77G: `true` when the current value is present-invalid (amber + flag). */
  invalid?: boolean;
  /** Short flag label shown when `invalid`; the full reason rides `title`. */
  invalidFlag?: string;
  /** Full invalid reason (tooltip / aria) — the backend validity message. */
  invalidReason?: string;
}

export function MetadataOptionSelect({
  value,
  options,
  onChange,
  disabled,
  noneLabel = NONE_LABEL,
  ariaLabel,
  invalid = false,
  invalidFlag,
  invalidReason,
}: OptionSelectProps) {
  const [open, setOpen] = useState(false);

  const pick = useCallback(
    (v: string | null) => {
      setOpen(false);
      if (v !== value) onChange(v);
    },
    [value, onChange],
  );

  const { showFlag, reasonId, describedBy } = useInvalidFlag(invalid, value != null, invalidReason);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label={ariaLabel}
          aria-expanded={open}
          aria-invalid={invalid || undefined}
          aria-describedby={describedBy}
          title={showFlag ? invalidReason : undefined}
          disabled={disabled}
          className={CTL_BASE}
          style={ctlStyle(invalid, disabled)}
        >
          <span className={value == null ? "text-[var(--muted-foreground)]" : ""}>
            {value ?? noneLabel}
          </span>
          {showFlag && (
            <InvalidFlagChip label={invalidFlag} reason={invalidReason} reasonId={reasonId} />
          )}
          <ChevronDown
            size={12}
            className={showFlag ? "" : "ml-auto"}
            style={{ color: "var(--muted-foreground)" }}
          />
        </button>
      </PopoverTrigger>
      <PopoverContent className={MENU_CONTENT_CLASS} style={popStyle}>
        <button
          type="button"
          className="flex w-full items-center gap-2 rounded px-2 py-1 text-[12px] hover:bg-black/5"
          style={{ color: "var(--muted-foreground)" }}
          onClick={() => pick(null)}
        >
          {noneLabel}
        </button>
        {options.map((o) => (
          <button
            key={o}
            type="button"
            className={`flex w-full items-center gap-2 rounded px-2 py-1 text-[12px] hover:bg-black/5 ${
              o === value ? "font-semibold" : ""
            }`}
            onClick={() => pick(o)}
          >
            {o}
          </button>
        ))}
      </PopoverContent>
    </Popover>
  );
}

// ---------------------------------------------------------------------------
// Status combobox — free-text input with live taxonomy suggestions.
// ---------------------------------------------------------------------------
interface StatusComboboxProps {
  value: string | null;
  /** Commit a value (blur/Enter/suggestion pick). null clears. */
  onChange: (value: string | null) => void;
  disabled?: boolean;
  ariaLabel: string;
}

export function MetadataStatusCombobox({
  value,
  onChange,
  disabled,
  ariaLabel,
}: StatusComboboxProps) {
  const [open, setOpen] = useState(false);
  const [draft, setDraftState] = useState(value ?? "");
  const draftRef = useRef(draft);
  const setDraft = useCallback((next: string) => {
    draftRef.current = next;
    setDraftState(next);
  }, []);
  const [activeIndex, setActiveIndex] = useState(-1);
  const states = useStatusTaxonomyStore((s) => s.states);
  const listId = useId();

  // The blur-commit is deferred (so a suggestion mousedown can win first). If the
  // band unmounts within that window (file switch / Esc), the pending timer must
  // not fire `commit` against a stale/unmounted tree — track and clear it.
  const blurTimerRef = useRef<number | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const clearBlurTimer = useCallback(() => {
    if (blurTimerRef.current !== null) {
      window.clearTimeout(blurTimerRef.current);
      blurTimerRef.current = null;
    }
  }, []);
  useEffect(() => clearBlurTimer, [clearBlurTimer]);

  const closeSuggestions = useCallback(() => {
    setOpen(false);
    setActiveIndex(-1);
  }, []);

  const commit = useCallback(
    (v: string) => {
      clearBlurTimer();
      closeSuggestions();
      const trimmed = v.trim();
      setDraft(trimmed);
      const next = trimmed === "" ? null : trimmed;
      if (next !== value) onChange(next);
    },
    [value, onChange, clearBlurTimer, closeSuggestions, setDraft],
  );

  const displayedDraft = open ? draft : (value ?? "");
  const filtered = useMemo(() => {
    const query = displayedDraft.trim().toLowerCase();
    return states.filter(
      (state) =>
        state.id.toLowerCase().includes(query) || state.label.toLowerCase().includes(query),
    );
  }, [states, displayedDraft]);

  const openSuggestions = () => {
    if (textMenuWithin(inputRef.current)) return;
    setDraft(value ?? "");
    setOpen(true);
    setActiveIndex(filtered.findIndex((state) => state.id === value));
  };

  const scrollActiveOptionIntoView = (option: HTMLButtonElement | null) => {
    const list = option?.closest('[role="listbox"]');
    if (!option || !list) return;
    // Scroll only the listbox so ancestor scroll containers cannot move the app shell.
    const listBounds = list.getBoundingClientRect();
    const optionBounds = option.getBoundingClientRect();
    if (optionBounds.top < listBounds.top) {
      list.scrollTop += optionBounds.top - listBounds.top;
    } else if (optionBounds.bottom > listBounds.bottom) {
      list.scrollTop += optionBounds.bottom - listBounds.bottom;
    }
  };

  return (
    // Radix owns outside-click + Escape dismissal via `onOpenChange(false)`; the
    // input still drives open on focus/click and its own Enter/Escape/blur logic
    // (a combobox, not a click-to-toggle picker).
    <Popover
      open={open}
      onOpenChange={(next) => {
        if (!next && !textMenuWithin(inputRef.current)) closeSuggestions();
      }}
    >
      <PopoverAnchor asChild>
        <div
          className={CTL_BASE}
          style={ctlStyle(false, disabled)}
          onClick={() => {
            if (!disabled && !open) openSuggestions();
          }}
        >
          {value && <StageRail status={value} labelled={false} />}
          <input
            ref={inputRef}
            role="combobox"
            aria-expanded={open}
            aria-controls={open ? listId : undefined}
            aria-activedescendant={
              open && filtered[activeIndex] ? `${listId}-${filtered[activeIndex].id}` : undefined
            }
            aria-autocomplete="list"
            aria-label={ariaLabel}
            disabled={disabled}
            value={displayedDraft}
            placeholder="(none)"
            className="min-w-0 flex-1 border-none bg-transparent outline-none"
            style={{ color: "var(--foreground)" }}
            onFocus={openSuggestions}
            onChange={(e) => {
              setDraft(e.target.value);
              setActiveIndex(-1);
            }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                if (!open) {
                  openSuggestions();
                } else if (filtered.length > 0) {
                  setActiveIndex((index) =>
                    e.key === "ArrowDown"
                      ? (index + 1) % filtered.length
                      : (index <= 0 ? filtered.length : index) - 1,
                  );
                }
              } else if (e.key === "Enter") {
                e.preventDefault();
                commit(filtered[activeIndex]?.id ?? draft);
              } else if (e.key === "Escape") {
                e.preventDefault();
                closeSuggestions();
                setDraft(value ?? "");
              }
            }}
            onBlur={(event) => {
              // Let a suggestion mousedown win before we commit the raw draft.
              // Tracked so an unmount within the window cancels the pending commit.
              clearBlurTimer();
              const field = event.currentTarget;
              commitTextEditOnBlur(field, () => {
                const text = draftRef.current;
                blurTimerRef.current = window.setTimeout(() => {
                  blurTimerRef.current = null;
                  commit(text);
                }, 100);
              });
            }}
          />
          <ChevronDown size={12} style={{ color: "var(--muted-foreground)" }} />
        </div>
      </PopoverAnchor>
      <PopoverContent
        id={listId}
        role="listbox"
        aria-label="Status suggestions"
        onOpenAutoFocus={(e) => e.preventDefault()}
        onInteractOutside={(event) => {
          if (inputRef.current?.contains(event.target as Node) || textMenuWithin(inputRef.current))
            event.preventDefault();
        }}
        className={MENU_CONTENT_CLASS}
        style={popStyle}
      >
        <div
          className="px-2 pt-1 pb-1 text-[10px] font-semibold tracking-wide uppercase"
          style={{ color: "var(--muted-foreground)" }}
        >
          suggestions · free-text ok
        </div>
        {filtered.length === 0 ? (
          <div className="px-2 py-1 text-[11px]" style={{ color: "var(--muted-foreground)" }}>
            No matches — Enter to keep &ldquo;{draft.trim()}&rdquo;
          </div>
        ) : (
          filtered.map((s, index) => (
            <button
              key={s.id}
              id={`${listId}-${s.id}`}
              ref={index === activeIndex ? scrollActiveOptionIntoView : undefined}
              type="button"
              role="option"
              aria-selected={index === activeIndex}
              className={menuOptionClass(index === activeIndex)}
              onClick={() => commit(s.id)}
              onMouseDown={(e) => {
                e.preventDefault();
              }}
            >
              <StageRailShape role={s.role} />
              <span>{s.label}</span>
              {s.id !== s.label.toLowerCase().replace(/\s+/g, "-") && (
                <span className="font-mono text-muted-foreground">{s.id}</span>
              )}
              <MenuCheck checked={s.id === value} />
            </button>
          ))
        )}
      </PopoverContent>
    </Popover>
  );
}

// ---------------------------------------------------------------------------
// Source-repo constrained picker (AC-5). Configured repos only; an invalid
// current value is pinned at the top, flagged, and still selectable/clearable.
// ---------------------------------------------------------------------------
interface SourceRepoPickerProps {
  value: string | null;
  /** Configured repos for the active team (attached sorted first by caller). */
  options: SourceRepoOption[];
  onChange: (value: string | null) => void;
  disabled?: boolean;
  /** `true` when the current `value` is present-invalid (malformed/not configured). */
  invalid: boolean;
  /** Flag label for the invalid pin, e.g. "not recognized" / "malformed". */
  invalidFlag?: string;
  ariaLabel: string;
}

export function MetadataSourceRepoPicker({
  value,
  options,
  onChange,
  disabled,
  invalid,
  invalidFlag = "not recognized",
  ariaLabel,
}: SourceRepoPickerProps) {
  const [open, setOpen] = useState(false);

  const pick = useCallback(
    (v: string | null) => {
      setOpen(false);
      if (v !== value) onChange(v);
    },
    [value, onChange],
  );

  // The invalid current value is pinned at the top and never dropped, even
  // though it isn't in the configured options (AC-5).
  const showPinned = invalid && value != null;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label={ariaLabel}
          aria-expanded={open}
          disabled={disabled}
          className={CTL_BASE}
          style={ctlStyle(invalid, disabled)}
        >
          <span className={`font-mono ${value == null ? "text-[var(--muted-foreground)]" : ""}`}>
            {value ?? "(none)"}
          </span>
          {invalid && value != null && <InvalidFlagChip label={invalidFlag} />}
          <ChevronDown
            size={12}
            className={invalid && value != null ? "" : "ml-auto"}
            style={{ color: "var(--muted-foreground)" }}
          />
        </button>
      </PopoverTrigger>
      <PopoverContent className={MENU_CONTENT_CLASS} style={popStyle}>
        {showPinned && (
          <button
            type="button"
            className="mb-0.5 flex w-full items-center gap-2 rounded border-b px-2 py-1 text-[12px] hover:bg-black/5"
            style={{ borderColor: "var(--border)" }}
            onClick={() => pick(value)}
          >
            <span className="font-mono">{value}</span>
            <InvalidFlagChip label={invalidFlag} />
          </button>
        )}
        <div
          className="flex items-center gap-1 px-2 pt-1 pb-1 text-[10px] font-semibold tracking-wide uppercase"
          style={{ color: "var(--muted-foreground)" }}
        >
          <Search size={10} />
          configured repos · attached first
        </div>
        {options.map((o) => (
          <button
            key={o.slug}
            type="button"
            className={`flex w-full items-center gap-2 rounded px-2 py-1 text-[12px] hover:bg-black/5 ${
              o.slug === value ? "font-semibold" : ""
            }`}
            onClick={() => pick(o.slug)}
          >
            <GitBranch size={12} className="flex-shrink-0" />
            <span className="truncate font-mono">{o.slug}</span>
            {o.attached && (
              <span className="ml-auto text-[10px]" style={{ color: "var(--muted-foreground)" }}>
                attached
              </span>
            )}
          </button>
        ))}
        <button
          type="button"
          className="flex w-full items-center gap-2 rounded px-2 py-1 text-[12px] hover:bg-black/5"
          style={{ color: "var(--muted-foreground)" }}
          onClick={() => pick(null)}
        >
          Clear (remove field)
        </button>
      </PopoverContent>
    </Popover>
  );
}

// ---------------------------------------------------------------------------
// Plain text field (story_id, branch) with blur-commit.
// ---------------------------------------------------------------------------
interface TextFieldProps {
  value: string | null;
  onChange: (value: string | null) => void;
  disabled?: boolean;
  ariaLabel: string;
  placeholder?: string;
  /** SW-AU-77G: `true` when the current value is present-invalid (amber + flag). */
  invalid?: boolean;
  /** Short flag label shown when `invalid`; the full reason rides `title`. */
  invalidFlag?: string;
  /** Full invalid reason (tooltip / aria) — the backend validity message. */
  invalidReason?: string;
}

export function MetadataTextField({
  value,
  onChange,
  disabled,
  ariaLabel,
  placeholder,
  invalid = false,
  invalidFlag,
  invalidReason,
}: TextFieldProps) {
  // Uncontrolled input keyed by the committed value: React remounts it (thus
  // re-seeds `defaultValue`) whenever the external value changes — e.g. after an
  // external reload — without a setState-in-effect. Local keystrokes stay in the
  // DOM until blur/Enter commit; committing changes `value`, which re-keys.
  const commit = useCallback(
    (raw: string) => {
      const trimmed = raw.trim();
      const next = trimmed === "" ? null : trimmed;
      if (next !== value) onChange(next);
    },
    [value, onChange],
  );

  const { showFlag, reasonId, describedBy } = useInvalidFlag(
    invalid,
    (value ?? "") !== "",
    invalidReason,
  );

  return (
    <div
      className={CTL_BASE}
      style={ctlStyle(invalid, disabled)}
      title={showFlag ? invalidReason : undefined}
    >
      <input
        key={value ?? ""}
        aria-label={ariaLabel}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy}
        disabled={disabled}
        defaultValue={value ?? ""}
        placeholder={placeholder}
        className="min-w-0 flex-1 border-none bg-transparent outline-none"
        style={{ color: "var(--foreground)" }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            (e.target as HTMLInputElement).blur();
          }
        }}
        onBlur={(event) => {
          const field = event.currentTarget;
          commitTextEditOnBlur(field, () => commit(field.value));
        }}
      />
      {showFlag && (
        <InvalidFlagChip label={invalidFlag} reason={invalidReason} reasonId={reasonId} />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Date fields for schema-declared tags.
// ---------------------------------------------------------------------------
interface DateFieldProps {
  value: string | null;
  onChange: (value: string | null) => void;
  disabled?: boolean;
  ariaLabel: string;
  /** SW-AU-77G: `true` when the current value is present-invalid (amber + flag). */
  invalid?: boolean;
  /** Short flag label shown when `invalid`; the full reason rides `title`. */
  invalidFlag?: string;
  /** Full invalid reason (tooltip / aria) — the backend validity message. */
  invalidReason?: string;
}

/**
 * Declared date tags can contain a full ISO-8601
 * timestamp (e.g. `2026-01-01T00:00:00Z`). An `<input type="date">` only accepts
 * a bare `YYYY-MM-DD` — feeding it a full timestamp makes the browser render the
 * control BLANK. So we display the date portion of whatever is stored (a doc
 * that HAS a date still shows it), and only overwrite the stored value
 * when the user actually edits the date (committing a `YYYY-MM-DD`). Loading a
 * doc never rewrites/downgrades its timestamp.
 */
function toDateInputValue(stored: string | null): string {
  if (!stored) return "";
  // Take the date portion of an ISO datetime; a bare `YYYY-MM-DD` passes
  // through unchanged.
  const datePart = stored.slice(0, 10);
  return /^\d{4}-\d{2}-\d{2}$/.test(datePart) ? datePart : "";
}

export function MetadataDateField({
  value,
  onChange,
  disabled,
  ariaLabel,
  invalid = false,
  invalidFlag,
  invalidReason,
}: DateFieldProps) {
  // A non-ISO value (`someday`) can't populate `<input type="date">` (the browser
  // renders it blank), so the amber flag + reason is how an invalid declared date
  // reads. The
  // bad value stays in the file (never silently wiped) until the user picks a date.
  const { showFlag, reasonId, describedBy } = useInvalidFlag(
    invalid,
    (value ?? "") !== "",
    invalidReason,
  );
  return (
    <div
      className={CTL_BASE}
      style={ctlStyle(invalid, disabled)}
      title={showFlag ? invalidReason : undefined}
    >
      <Calendar size={12} style={{ color: "var(--muted-foreground)" }} />
      <input
        type="date"
        aria-label={ariaLabel}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy}
        disabled={disabled}
        value={toDateInputValue(value)}
        className="min-w-0 flex-1 border-none bg-transparent outline-none"
        style={{ color: "var(--foreground)" }}
        onChange={(e) => {
          const v = e.target.value;
          onChange(v === "" ? null : v);
        }}
      />
      {showFlag && (
        <InvalidFlagChip label={invalidFlag} reason={invalidReason} reasonId={reasonId} />
      )}
    </div>
  );
}
