// SPDX-License-Identifier: MIT
// The ONE typed value control for a declared field (team custom OR org tag), chosen by
// declared type. Extracted from the metadata drawer's inline dispatch so the drawer AND
// the SW-AU-78 composer rule builder select the same control, in the same declared
// order, with the same invalid treatment — one table, two consumers (AC-16). An operator
// or control change lands here once and both surfaces move together.

import {
  MetadataOptionSelect,
  MetadataTextField,
  MetadataDateField,
} from "./metadata-field-controls";
import { MetadataListChipEditor } from "./metadata-list-chip-editor";

const YES_NO_OPTIONS = ["yes", "no"] as const;

/**
 * Canonicalize a declared row's type the way the backend value validator does (`trim` +
 * `toLowerCase`, folding the `yes_no` underscore alias to the kebab `yes-no`) so the FE
 * control + flag never diverge from the verdict on a hand-authored casing (`Choice`,
 * `DATE`, ` yes-no `). Handles BOTH vocabularies: the team `FieldSchema`
 * (`choice`/`yes-no`/`number`/`date`/`text`) and the org `TagType`
 * (`select`/`multi_select`/`ordered`/`status`/`date`/`relation`/`text`/`unknown`). The
 * loader stores `type:` verbatim, so this lives at the consumer.
 */
export function normalizeDeclaredType(raw: string | undefined): string | undefined {
  const t = raw?.trim().toLowerCase();
  return t === "yes_no" ? "yes-no" : t;
}

/** Does this normalized type render as a single closed-set select? Team `choice` and org
 *  `select`/`ordered`/`status` all pick one value from a vocabulary (the `ordered`
 *  ladder is display-only — options already arrive low→high). */
export function isChoiceType(type: string | undefined): boolean {
  return type === "choice" || type === "select" || type === "ordered" || type === "status";
}

/**
 * The short inline flag label per declared-row type (the full reason is the backend
 * validity `message`, screen-reader announced by the control). Chrome only; the verdict
 * is the backend's. `type` is already normalized by {@link normalizeDeclaredType}.
 */
export function declaredInvalidFlag(type: string | undefined): string {
  if (isChoiceType(type) || type === "multi_select") return "not allowed";
  switch (type) {
    case "number":
      return "not a number";
    case "date":
      return "use YYYY-MM-DD";
    case "yes-no":
      return "use yes or no";
    default:
      return "invalid";
  }
}

export interface TagValueControlProps {
  /** The raw declared type (team `FieldSchema` or org `TagType`); normalized here. */
  type: string | undefined;
  /** Current values — scalar controls read `[0]`; `multi_select` reads the whole list. */
  values: string[];
  /** Ordered allowed-value vocabulary for a `choice`/`select` field (declared order). */
  allowedValues?: string[];
  ariaLabel: string;
  disabled?: boolean;
  invalid?: boolean;
  /** The full validity reason, screen-reader announced by the control. */
  invalidReason?: string;
  /** Commit a scalar value (`null` clears). */
  onChangeScalar: (value: string | null) => void;
  /** Commit a `multi_select` per-item list. */
  onChangeList: (items: string[]) => void;
}

/**
 * Render the typed control for a declared field value. Deterministic dispatch on the
 * normalized type: `multi_select` → chip editor; `choice`/`select`/`ordered`/`status` →
 * option select over `allowedValues` (declared order); `yes-no` → yes/no select; `date`
 * → date field; everything else → free text (validity still flags a bad value).
 */
export function TagValueControl({
  type: rawType,
  values,
  allowedValues,
  ariaLabel,
  disabled,
  invalid,
  invalidReason,
  onChangeScalar,
  onChangeList,
}: TagValueControlProps) {
  const type = normalizeDeclaredType(rawType);
  const shared = {
    ariaLabel,
    disabled,
    invalid,
    invalidFlag: declaredInvalidFlag(type),
    invalidReason,
  };
  if (type === "multi_select") {
    return <MetadataListChipEditor items={values} onChange={onChangeList} {...shared} />;
  }
  const scalar = values[0] ?? "";
  if (isChoiceType(type)) {
    return (
      <MetadataOptionSelect
        value={scalar}
        options={allowedValues ?? []}
        noneLabel="(none)"
        onChange={onChangeScalar}
        {...shared}
      />
    );
  }
  if (type === "yes-no") {
    return (
      <MetadataOptionSelect
        value={scalar}
        options={YES_NO_OPTIONS}
        noneLabel="(none)"
        onChange={onChangeScalar}
        {...shared}
      />
    );
  }
  if (type === "date") {
    return <MetadataDateField value={scalar} onChange={onChangeScalar} {...shared} />;
  }
  return <MetadataTextField value={scalar} onChange={onChangeScalar} {...shared} />;
}
