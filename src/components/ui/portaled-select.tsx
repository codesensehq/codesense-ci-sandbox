// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useRef, useState } from "react";

import { Check, ChevronDown } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

/** One selectable option. A `disabled` option renders but can't be picked (mirrors a native
 *  `<option disabled>`). */
export interface PortaledSelectOption {
  value: string;
  label: string;
  disabled?: boolean;
  /** Shown before the label, in the list and on the trigger. */
  icon?: React.ReactNode;
  /** Shown after the label, in the list and on the trigger (a badge such as Preview). It is not
   *  focusable; anything it says that its text doesn't goes in `describedBy`. */
  adornment?: React.ReactNode;
  /** Id of an element that describes the option, on the option and, chosen, on the trigger. */
  describedBy?: string;
}

/** An optional labelled group of options (mirrors a native `<optgroup>`). */
export interface PortaledSelectGroup {
  label: string;
  options: PortaledSelectOption[];
}

export interface PortaledSelectProps {
  /** The current value; matched against the flattened option list to show its label + the check. */
  value: string;
  /** Emits the picked option's `value` — the direct analogue of `onChange={e => fn(e.target.value)}`. */
  onValueChange: (value: string) => void;
  /** Flat option list. Ignored when `groups` is supplied. */
  options?: PortaledSelectOption[];
  /** Grouped option list (mutually exclusive with `options`). */
  groups?: PortaledSelectGroup[];
  /** Shown on the trigger when the current value matches no option (e.g. "Choose a workflow…"). */
  placeholder?: string;
  /** Accessible name for the trigger combobox (`getByRole("combobox", { name })`). */
  ariaLabel?: string;
  /** Trigger id (for an external `<label htmlFor>` association). */
  id?: string;
  /** Id of an external element that labels the trigger. */
  ariaLabelledby?: string;
  /** Id of an external element that describes the trigger. */
  ariaDescribedby?: string;
  disabled?: boolean;
  /** Extra classes on the listbox content. */
  className?: string;
  /** Extra classes on the trigger button. */
  triggerClassName?: string;
}

/** The shared field styling for the trigger button, so a `PortaledSelect` sits flush beside the
 *  native inputs it replaces. */
const TRIGGER_CLASS =
  "flex h-7 w-full items-center justify-between gap-2 rounded border border-border bg-background px-2 text-left text-xs outline-none focus:border-ring disabled:opacity-50";

/**
 * A portaled dropdown select built on the Radix `Popover` (NOT the Radix `Select` primitive, whose
 * jsdom/pointer-capture behavior is painful to test, and NOT a native `<select>`, whose option popup
 * detaches and mis-positions when an ancestor has a CSS transform — a React Flow canvas or a dialog).
 * The `PopoverContent` portals to `document.body`, so the option list always positions correctly.
 *
 * Accessible + testable: the trigger is a `role="combobox"` with `aria-haspopup="listbox"`; the open
 * content is `role="listbox"` and each option is `role="option"` with `aria-selected`. Tests drive it
 * with `getByRole("combobox", { name })` + `getByRole("option", { name })`.
 */
export function PortaledSelect({
  value,
  onValueChange,
  options,
  groups,
  placeholder,
  ariaLabel,
  id,
  ariaLabelledby,
  ariaDescribedby,
  disabled,
  className,
  triggerClassName,
}: PortaledSelectProps): React.JSX.Element {
  const [open, setOpen] = useState(false);
  // Which option the keyboard cursor is on (index into the flattened list); -1 = none highlighted.
  const [activeIndex, setActiveIndex] = useState(-1);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // The keyboard cursor stays in sight: a long list scrolls to follow it.
  useEffect(() => {
    if (!open || activeIndex < 0) return;
    listRef.current
      ?.querySelector(`[data-index="${activeIndex}"]`)
      ?.scrollIntoView?.({ block: "nearest" });
  }, [open, activeIndex]);

  // Flatten groups → a single ordered list so value-matching, the arrow cursor, and Enter-select
  // all reason over one array regardless of whether the caller passed `options` or `groups`.
  const flatOptions = useMemo<PortaledSelectOption[]>(
    () => (groups ? groups.flatMap((g) => g.options) : (options ?? [])),
    [groups, options],
  );
  const selected = flatOptions.find((o) => o.value === value);
  const selectableIndexes = useMemo(
    () => flatOptions.map((o, i) => (o.disabled ? -1 : i)).filter((i) => i >= 0),
    [flatOptions],
  );

  const select = (option: PortaledSelectOption): void => {
    if (option.disabled) return;
    onValueChange(option.value);
    setOpen(false);
    // Return focus to the trigger so keyboard flow continues from the control, like a native select.
    triggerRef.current?.focus();
  };

  const openWith = (index: number): void => {
    setActiveIndex(index);
    setOpen(true);
  };

  // Open on Enter/Space/ArrowDown (like a native select); when open, arrow-navigate + Enter-select.
  // Shared by the trigger button and the open listbox, so it takes the common Element type.
  const onTriggerKeyDown = (e: React.KeyboardEvent<Element>): void => {
    if (disabled) return;
    if (!open) {
      if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const start =
          selectableIndexes.find((i) => i === flatOptions.indexOf(selected ?? flatOptions[0])) ??
          selectableIndexes[0] ??
          -1;
        openWith(start);
      }
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      const pos = selectableIndexes.indexOf(activeIndex);
      const next =
        selectableIndexes[Math.min(pos + 1, selectableIndexes.length - 1)] ?? activeIndex;
      setActiveIndex(next);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const pos = selectableIndexes.indexOf(activeIndex);
      const prev = selectableIndexes[Math.max(pos - 1, 0)] ?? activeIndex;
      setActiveIndex(prev);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      const option = flatOptions[activeIndex];
      if (option) select(option);
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  };

  let flatCursor = -1;
  const renderOption = (option: PortaledSelectOption): React.JSX.Element => {
    flatCursor += 1;
    const index = flatCursor;
    const isSelected = option.value === value;
    return (
      <button
        key={`${option.value}-${index}`}
        type="button"
        role="option"
        data-value={option.value}
        data-index={index}
        aria-selected={isSelected}
        aria-disabled={option.disabled || undefined}
        aria-describedby={option.describedBy}
        disabled={option.disabled}
        tabIndex={-1}
        onClick={() => select(option)}
        onMouseEnter={() => !option.disabled && setActiveIndex(index)}
        onPointerDown={(e) => e.stopPropagation()}
        className={cn(
          "flex w-full items-center justify-between gap-2 rounded px-2 py-1 text-left text-xs",
          option.disabled ? "cursor-not-allowed opacity-50" : "hover:bg-accent",
          index === activeIndex && !option.disabled && "bg-accent",
          isSelected && "font-medium",
        )}
      >
        <span className="flex min-w-0 items-center gap-1.5">
          {option.icon}
          <span className="truncate">{option.label}</span>
          {option.adornment}
        </span>
        {isSelected && <Check aria-hidden className="size-3.5 shrink-0" />}
      </button>
    );
  };

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        if (disabled) return;
        setOpen(next);
      }}
    >
      <PopoverTrigger asChild>
        <button
          ref={triggerRef}
          type="button"
          id={id}
          role="combobox"
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-label={ariaLabel}
          aria-labelledby={ariaLabelledby}
          aria-describedby={
            [ariaDescribedby, selected?.describedBy].filter(Boolean).join(" ") || undefined
          }
          disabled={disabled}
          onKeyDown={onTriggerKeyDown}
          onPointerDown={(e) => e.stopPropagation()}
          className={cn(TRIGGER_CLASS, triggerClassName)}
        >
          <span className="flex min-w-0 items-center gap-1.5">
            {selected?.icon}
            <span className={cn("truncate", !selected && "text-muted-foreground")}>
              {selected?.label ?? placeholder ?? "—"}
            </span>
            {selected?.adornment}
          </span>
          <ChevronDown aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
        </button>
      </PopoverTrigger>
      <PopoverContent
        ref={listRef}
        role="listbox"
        align="start"
        // Match the trigger width and cap the height so a long list scrolls instead of overflowing.
        className={cn(
          "max-h-[40vh] w-[var(--radix-popover-trigger-width)] overflow-auto p-1",
          className,
        )}
        onKeyDown={onTriggerKeyDown}
      >
        {flatOptions.length === 0 ? (
          <p className="px-2 py-1 text-xs text-muted-foreground">No options.</p>
        ) : groups ? (
          groups.map((group) => (
            <div key={group.label} role="group" aria-label={group.label}>
              <p className="px-2 pb-0.5 pt-1 text-[10px] uppercase tracking-wide text-muted-foreground">
                {group.label}
              </p>
              {group.options.map(renderOption)}
            </div>
          ))
        ) : (
          flatOptions.map(renderOption)
        )}
      </PopoverContent>
    </Popover>
  );
}
