// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import { useActiveOrg } from "@/shared/stores/use-active-org";
import type { StablePerson } from "@/shared/types/domain";
import { personStableId } from "@/shared/lib/person-reference";

export function useRosterPeople(): readonly StablePerson[] {
  const people = useActiveOrg()?.people;
  return useMemo(
    () =>
      (people ?? []).map((person) => ({
        ...person,
        id: personStableId(person),
        identities: person.identities ?? [],
      })),
    [people],
  );
}
