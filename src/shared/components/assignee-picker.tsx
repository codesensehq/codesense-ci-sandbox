// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, Search } from "lucide-react";

import type { Person } from "@/shared/types/domain";
import { cn } from "@/lib/utils";
import { usePeopleByLogin } from "@/shared/stores/use-people-by-login";
import { useRosterPeople } from "@/shared/stores/use-roster-people";
import { useProviderCatalog } from "@/shared/hooks/use-provider-catalog";
import {
  buildPeopleById,
  personReference,
  resolvePersonReference,
} from "@/shared/lib/person-reference";
import { presentPerson } from "@/shared/lib/person-presentation";
import { PersonAvatar } from "@/shared/components/person-avatar";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { MENU_STYLE } from "@/shared/components/floating-menu";

// ---------------------------------------------------------------------------
// SW-ID-5 · The ONE assignee picker (assign/reassign everywhere a story shows).
//
// Built once, mounted on all three surfaces — metadata band, board card,
// pipeline node — so assignment can never fork into three parallel controls.
// It reads the SW-ID-1 roster itself (`usePeopleByLogin`) so callers pass only
// `value` + `onChange`; resolves + highlights the current selection by
// `usernameKey`-folded login (the SW-ID-6 dedup lesson); and owns its open
// state + the trigger's `stopPropagation` guard in ONE place, so the board card
// (@dnd-kit) and pipeline node never re-implement it. Display-only avatars come
// from the shared `PersonAvatar` (SW-ID-6) — this adds the WRITE half.
// SW-FX-79: the shell is now the portaled Radix `Popover` (no more
// `absolute top-full` clipping); contents + selection logic unchanged.
// ---------------------------------------------------------------------------

const popStyle = { ...MENU_STYLE, color: "var(--foreground)" } as const;

const ROW_BASE =
  "flex w-full items-center gap-2 rounded px-2 py-1 text-left text-[12.5px] hover:bg-black/5";

export interface AssigneePickerProps {
  /** Current assignee login (raw, as stored in `assigned_to`); null = unassigned. */
  value: string | null;
  /** Commit a selection: a roster login, or null for **Unassigned**. */
  onChange: (login: string | null) => void;
  disabled?: boolean;
  ariaLabel?: string;
  /**
   * Custom trigger (the `AssigneeAvatar` on the board card / pipeline node).
   * Omitted → a default labelled button (avatar + name + chevron) for the
   * metadata band. Either way the picker wraps it in the toggle button that owns
   * the `stopPropagation` guard, so no surface re-implements it.
   */
  children?: React.ReactNode;
  /** Extra classes for the anchor wrapper (positioning at the mount site). */
  className?: string;
}

export function AssigneePicker({
  value,
  onChange,
  disabled,
  ariaLabel = "Assignee",
  children,
  className,
}: AssigneePickerProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");

  const index = usePeopleByLogin();
  const activeRoster = useRosterPeople();
  const providerCatalog = useProviderCatalog();
  const roster = useMemo(
    () => (activeRoster.length > 0 ? activeRoster : [...index.values()]),
    [activeRoster, index],
  );
  const byId = useMemo(() => buildPeopleById(roster), [roster]);
  // Treat a blank/whitespace stored value as Unassigned — the picker never writes
  // `""`, but a hand-authored `assigned_to: ""` should highlight Unassigned rather
  // than nothing (the scanner already reads it as unassigned everywhere else).
  const assignedLogin = value && value.trim() !== "" ? value : null;
  const currentPerson = resolvePersonReference(assignedLogin, byId, index);
  const currentKey = currentPerson ? personReference(currentPerson) : (assignedLogin ?? "");

  // Radix owns outside-click + Escape dismissal now; reset the filter each open.
  const handleOpenChange = useCallback((next: boolean) => {
    if (next) setQuery("");
    setOpen(next);
  }, []);

  // Roster as a name-sorted list. The index is a memoized Map (no IPC), so this
  // filter/sort is over an in-memory array — never blocks the render.
  // SW-FG-4b (AC-7): index values always carry a non-null handle (a handle-less
  // person never enters `buildPeopleByLogin`), so the `?? ""` coalescing below is
  // a pure type-guard for `github_username: string | null` — never hit at runtime.
  const people = useMemo(() => {
    const all = [...roster];
    all.sort((a, b) =>
      (a.display_name || a.github_username || "").localeCompare(
        b.display_name || b.github_username || "",
      ),
    );
    return all;
  }, [roster]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q === "") return people;
    return people.filter(
      (p) =>
        p.display_name.toLowerCase().includes(q) ||
        (p.github_username ?? "").toLowerCase().includes(q) ||
        (p.identities ?? []).some((identity) => (identity.handle ?? "").toLowerCase().includes(q)),
    );
  }, [people, query]);

  // Focus the typeahead input when the popover opens (the query is reset in
  // `handleOpenChange`, so this effect stays a pure DOM side-effect — no setState).
  // Radix's default open-auto-focus is suppressed on the content so this is the
  // only focus move on open.
  useEffect(() => {
    if (open) inputRef.current?.focus();
  }, [open]);

  const pick = useCallback(
    (login: string | null) => {
      setOpen(false);
      // Compare on the folded key so re-picking the same person under different
      // casing is a no-op (never a redundant write).
      const nextKey = login ?? "";
      if (nextKey !== currentKey) onChange(login);
    },
    [currentKey, onChange],
  );

  const label = currentPerson?.display_name || (assignedLogin ? `@${assignedLogin}` : "Unassigned");

  return (
    // When a host reveals the trigger via `opacity-0 ... group-hover` (the pipeline
    // node's unassigned trigger), while OPEN we force full opacity via inline style
    // (beats the Tailwind class), otherwise the open list could fade out mid-
    // interaction if hover/focus is lost.
    <div className={cn(className)} style={open && !disabled ? { opacity: 1 } : undefined}>
      <Popover open={open && !disabled} onOpenChange={handleOpenChange}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label={ariaLabel}
            disabled={disabled}
            // The trigger owns the guard for ALL surfaces: clicking the avatar on a
            // board card / pipeline node must not open the story or start a drag.
            onClick={(e) => e.stopPropagation()}
            className={cn(
              "flex items-center outline-none cursor-pointer disabled:cursor-not-allowed disabled:opacity-55",
              // Default (band) trigger is a bordered control; a custom child (the
              // board/pipeline avatar) needs no chrome of its own.
              !children && "w-full gap-1.5 rounded-md border px-2 py-1 text-[12.5px]",
            )}
            style={
              children
                ? undefined
                : {
                    backgroundColor: "var(--popover, var(--card))",
                    borderColor: "var(--border)",
                    color: "var(--foreground)",
                  }
            }
          >
            {children ?? (
              <>
                <PersonAvatar person={currentPerson} login={assignedLogin} size="sm" />
                <span className={assignedLogin == null ? "text-[var(--muted-foreground)]" : ""}>
                  {label}
                </span>
                <ChevronDown
                  size={12}
                  className="ml-auto"
                  style={{ color: "var(--muted-foreground)" }}
                />
              </>
            )}
          </button>
        </PopoverTrigger>
        {/* Plain-button rows in a dismissable popover — the same pattern as the
            sibling `MetadataOptionSelect` / `RepoSelectorPopover` (Tab-navigable,
            current row bolded), deliberately NOT a `role=listbox` we don't fully
            drive with arrow-key/aria-activedescendant semantics.
            Stop clicks inside the popover (filter input, option rows) from bubbling
            to a host card / node open-handler. */}
        <PopoverContent
          align="end"
          side="bottom"
          onOpenAutoFocus={(e) => e.preventDefault()}
          onClick={(e) => e.stopPropagation()}
          className="w-[240px] max-h-64 gap-0 overflow-hidden rounded-md p-0 shadow-lg ring-0"
          style={popStyle}
        >
          <div className="flex items-center gap-1.5 border-b px-2 py-1" style={popStyle}>
            <Search size={12} style={{ color: "var(--muted-foreground)" }} />
            <input
              ref={inputRef}
              aria-label="Filter people"
              value={query}
              placeholder="Filter…"
              className="min-w-0 flex-1 border-none bg-transparent text-[12.5px] outline-none"
              style={{ color: "var(--foreground)" }}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <div className="max-h-52 overflow-y-auto p-1">
            <button
              type="button"
              className={cn(ROW_BASE, assignedLogin == null && "font-semibold")}
              style={{ color: "var(--muted-foreground)" }}
              onClick={() => pick(null)}
            >
              <PersonAvatar person={null} login={null} size="sm" />
              Unassigned
            </button>
            {/* Current assignee whose login isn't in the roster (left the org /
                hand-authored typo): show it as a bolded current row so the open
                list always marks who's assigned — the trigger label alone would
                otherwise leave the open list with nothing highlighted. Gated on an
                empty filter (it's not a roster match). Re-picking it is a no-op. */}
            {assignedLogin && !currentPerson && query.trim() === "" && (
              <button
                type="button"
                className={cn(ROW_BASE, "font-semibold")}
                onClick={() => pick(assignedLogin)}
              >
                <PersonAvatar person={null} login={assignedLogin} size="sm" />
                <span className="min-w-0 flex-1 truncate">
                  {assignedLogin.startsWith("person:") ? "Unknown person" : `@${assignedLogin}`}
                </span>
                <span className="shrink-0 text-[11px] text-[var(--muted-foreground)]">
                  not in roster
                </span>
              </button>
            )}
            {filtered.map((p: Person) => {
              const reference = personReference(p);
              const presentation = presentPerson(p, providerCatalog.labels);
              const selected = reference === currentKey;
              return (
                <button
                  key={presentation.id}
                  type="button"
                  className={cn(ROW_BASE, selected && "font-semibold")}
                  onClick={() => pick(reference)}
                >
                  <PersonAvatar person={p} login={p.github_username} size="sm" />
                  <span className="min-w-0 flex-1 truncate">{presentation.label}</span>
                  <span className="shrink-0 text-[11px] text-[var(--muted-foreground)]">
                    {presentation.secondaryHandle
                      ? `@${presentation.secondaryHandle}`
                      : presentation.linked
                        ? "Code host linked"
                        : "No code host linked"}
                  </span>
                </button>
              );
            })}
            {filtered.length === 0 && query.trim() !== "" && (
              <div
                className="px-2 py-2 text-center text-[12px]"
                style={{ color: "var(--muted-foreground)" }}
              >
                No people match “{query}”
              </div>
            )}
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}
