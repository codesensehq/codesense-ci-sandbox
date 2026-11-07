// SPDX-License-Identifier: MIT
import { useState } from "react";

import type { FieldType } from "@/tauri/commands";
import type { DeclareFieldArgs } from "@/shared/hooks/use-declare-team-field";
import { canonicalizeFieldName, isValidFieldName } from "@/shared/lib/field-name";
import { MetadataListChipEditor } from "./metadata-list-chip-editor";

/** The five authoring types, in menu order (SW-AU-77E AC-1). */
const FIELD_TYPES: readonly FieldType[] = ["text", "number", "yes-no", "choice", "date"];

interface AddCustomFieldFormProps {
  /** When set, declaring is not allowed here (AC-5) — the form is disabled and shows this
   *  truthful reason instead of a silent no-op. */
  disabledReason?: string | null;
  /** Declare the field (and optionally set its value on the current story). Rejects with a
   *  message on backend refusal (bad value, non-writable tree, …). */
  onDeclare: (args: DeclareFieldArgs) => Promise<void>;
  /** Dismiss the form (Cancel, or after a successful declare). */
  onClose: () => void;
}

/**
 * SW-AU-77E · Declare a NEW typed custom field from the story metadata drawer. A
 * deliberate name + type (never auto-created by typing an undeclared key — AC-1); a
 * `choice` field carries an allowed-value vocabulary (reusing the shared chip editor). The
 * optional value is set on the current story in the same action (AC-6). All persistence is
 * async off the webview thread via the injected `onDeclare` — this component never blocks
 * render (AC-9). When declaring isn't permitted (AC-5) the controls are disabled with the
 * truthful `disabledReason`.
 */
export function AddCustomFieldForm({
  disabledReason,
  onDeclare,
  onClose,
}: AddCustomFieldFormProps) {
  const [name, setName] = useState("");
  const [type, setType] = useState<FieldType>("text");
  const [allowedValues, setAllowedValues] = useState<string[]>([]);
  const [value, setValue] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const blocked = Boolean(disabledReason);
  const choiceIncomplete = type === "choice" && allowedValues.length === 0;
  const canSubmit = !blocked && !submitting && name.trim().length > 0 && !choiceIncomplete;

  const submit = async () => {
    if (!canSubmit) return;
    // A non-identifier name (spaces, punctuation) is rejected with guidance rather than a
    // silently-disabled button, so the user learns why.
    if (!isValidFieldName(name)) {
      setError("Use a simple name: lowercase letters, numbers, or underscores.");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await onDeclare({
        name: canonicalizeFieldName(name),
        type,
        allowedValues: type === "choice" ? allowedValues : [],
        value: value.trim() || null,
      });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="flex flex-col gap-2 p-1.5" style={{ minWidth: 220 }}>
      {blocked && (
        <div className="text-[11px]" role="note" style={{ color: "var(--muted-foreground)" }}>
          {disabledReason}
        </div>
      )}

      <input
        aria-label="new field name"
        className="rounded border px-2 py-1 text-[12px]"
        style={{ borderColor: "var(--border)" }}
        placeholder="field name"
        value={name}
        disabled={blocked || submitting}
        onChange={(e) => setName(e.target.value)}
      />

      <select
        aria-label="new field type"
        className="rounded border px-2 py-1 text-[12px]"
        style={{ borderColor: "var(--border)" }}
        value={type}
        disabled={blocked || submitting}
        onChange={(e) => setType(e.target.value as FieldType)}
      >
        {FIELD_TYPES.map((t) => (
          <option key={t} value={t}>
            {t}
          </option>
        ))}
      </select>

      {type === "choice" && (
        <MetadataListChipEditor
          ariaLabel="allowed values"
          items={allowedValues}
          disabled={blocked || submitting}
          onChange={setAllowedValues}
        />
      )}

      <input
        aria-label="new field value"
        className="rounded border px-2 py-1 text-[12px]"
        style={{ borderColor: "var(--border)" }}
        placeholder="value (optional)"
        value={value}
        disabled={blocked || submitting}
        onChange={(e) => setValue(e.target.value)}
      />

      {error && (
        <div className="text-[11px] text-destructive" role="alert">
          {error}
        </div>
      )}

      <div className="flex items-center justify-end gap-2">
        <button
          type="button"
          className="rounded px-2 py-1 text-[11.5px]"
          style={{ color: "var(--muted-foreground)" }}
          disabled={submitting}
          onClick={onClose}
        >
          Cancel
        </button>
        <button
          type="button"
          data-testid="declare-field-submit"
          className="rounded border px-2.5 py-1 text-[11.5px]"
          style={{ borderColor: "var(--border)", color: "var(--foreground)" }}
          disabled={!canSubmit}
          onClick={() => void submit()}
        >
          Add field
        </button>
      </div>
    </div>
  );
}
