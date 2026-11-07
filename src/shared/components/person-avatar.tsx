// SPDX-License-Identifier: MIT
import { useState } from "react";
import { User } from "lucide-react";

import type { Person } from "@/shared/types/domain";
import { cn } from "@/lib/utils";
import { stripUsernameInput } from "@/shared/lib/github-username";
import { useMemo } from "react";
import { usePeopleByLogin } from "@/shared/stores/use-people-by-login";
import { useRosterPeople } from "@/shared/stores/use-roster-people";
import { buildPeopleById, resolvePersonReference } from "@/shared/lib/person-reference";
import { presentPerson } from "@/shared/lib/person-presentation";
import { InfoTip } from "@/components/ui/tooltip";

// ---------------------------------------------------------------------------
// SW-ID-6 · The ONE person renderer (avatars everywhere)
//
// Replaces the two divergent passive person chips — `AssigneeChip` (2-char
// initials off a raw login) and `MemberAvatar` (Person → avatar_url, 1-char
// initial). A single `PersonAvatar` renders: real `avatar_url` image → initials
// fallback → a neutral "unassigned" glyph, with an identity tooltip (AC-1/AC-3).
// Pure display: it never fetches; a null `avatar_url` (or a login that resolves
// to no Person) simply falls back to initials.
// ---------------------------------------------------------------------------

export type PersonAvatarSize = "sm" | "md";

const SIZES: Record<PersonAvatarSize, { box: string; text: string; px: number }> = {
  sm: { box: "h-4 w-4", text: "text-[9px]", px: 16 },
  md: { box: "h-5 w-5", text: "text-[10px]", px: 20 },
};

/**
 * Up to two initials for a person, one rule for every surface — replacing
 * `AssigneeChip`'s 2-char-of-login and `MemberAvatar`'s 1-char-of-name. Prefers
 * the display name (first letters of its first two words; a single word → its
 * first two letters), and falls back to the login (`@`-stripped) when there is no
 * name. Reuses `stripUsernameInput` so a raw `@login` never leaks an `@`.
 */
export function personInitials(person: Person | null, login: string | null): string {
  const name = person?.display_name?.trim();
  if (name) {
    const words = name.split(/\s+/);
    const picked = words.length >= 2 ? words[0][0] + words[1][0] : name.slice(0, 2);
    return picked.toUpperCase();
  }
  const handle = stripUsernameInput(person?.github_username || login || "");
  return handle.slice(0, 2).toUpperCase();
}

/**
 * A person's avatar. `person` is the resolved roster entry (or `null` when a
 * login doesn't resolve); `login` is the raw assignee handle used for the
 * initials/tooltip fallback. A `null`/blank `login` with no `person` renders the
 * neutral **unassigned** placeholder (a muted `User` glyph) — never a blank box.
 */
export function PersonAvatar({
  person,
  login,
  size = "md",
  className,
}: {
  person: Person | null;
  login: string | null;
  size?: PersonAvatarSize;
  className?: string;
}) {
  // A remote avatar that 403s / fails degrades to initials rather than a broken
  // image icon (mirrors account-section.tsx). Tracked by the URL that FAILED (not
  // a bare boolean) so a later resolve to a different/working `avatar_url` on the
  // same reused instance re-attempts instead of staying stuck on initials.
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  const s = SIZES[size];

  const hasLogin = stripUsernameInput(login ?? "") !== "";
  const unassigned = !person && !hasLogin;
  const avatarUrl = person?.avatar_url ?? null;
  const showImg = !!avatarUrl && failedUrl !== avatarUrl;

  const tooltip = unassigned
    ? "Unassigned"
    : person
      ? (() => {
          const presentation = presentPerson(person);
          return presentation.secondaryHandle
            ? `${presentation.label} @${presentation.secondaryHandle}`
            : presentation.linked
              ? presentation.label
              : `${presentation.label} — No code host linked`;
        })()
      : `@${stripUsernameInput(login ?? "")}`;

  const trigger = unassigned ? (
    <span
      className={cn(
        "flex items-center justify-center rounded-full border border-dashed border-border bg-muted/40 text-muted-foreground",
        s.box,
        className,
      )}
    >
      <User size={s.px - 8} aria-hidden />
    </span>
  ) : showImg ? (
    <img
      src={avatarUrl}
      alt=""
      width={s.px}
      height={s.px}
      // async/off-thread load; fixed dims so it never reflows the card; GitHub
      // avatars want no-referrer; not draggable (the board card is @dnd-kit).
      loading="lazy"
      decoding="async"
      referrerPolicy="no-referrer"
      draggable={false}
      onError={() => setFailedUrl(avatarUrl)}
      className={cn("rounded-full object-cover", s.box, className)}
    />
  ) : (
    <span
      className={cn(
        "flex items-center justify-center rounded-full border border-border bg-muted font-medium uppercase text-muted-foreground",
        s.box,
        s.text,
        className,
      )}
    >
      {personInitials(person, login)}
    </span>
  );

  // SW-FX-80: inherits the single app-root TooltipProvider (delay=0), so the
  // avatar's identity tooltip works on every mount point without a local provider.
  return (
    <InfoTip label={tooltip} side="top">
      {trigger}
    </InfoTip>
  );
}

/**
 * Resolve-and-render for surfaces that carry only a login string (board cards,
 * pipeline nodes, swimlane rollups, the story metadata header): looks the login
 * up in the shared SW-ID-1 index and renders {@link PersonAvatar}. Keeps the
 * `usePeopleByLogin` + `personByLogin` resolution in ONE place so call sites just
 * pass a login. `null`/blank login → the neutral unassigned state.
 */
export function AssigneeAvatar({
  login,
  size,
  className,
}: {
  login: string | null;
  size?: PersonAvatarSize;
  className?: string;
}) {
  const index = usePeopleByLogin();
  const activeRoster = useRosterPeople();
  const people = useMemo(
    () => (activeRoster.length > 0 ? activeRoster : [...index.values()]),
    [activeRoster, index],
  );
  const byId = useMemo(() => buildPeopleById(people), [people]);
  const person = resolvePersonReference(login, byId, index);
  return <PersonAvatar person={person} login={login} size={size} className={className} />;
}
