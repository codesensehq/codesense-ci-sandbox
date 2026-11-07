// SPDX-License-Identifier: MIT
import * as React from "react";

import { cn } from "@/lib/utils";

import { Label } from "./label";

interface FieldProps extends Omit<React.ComponentProps<"div">, "children"> {
  label: React.ReactNode;
  hint?: React.ReactNode;
  /** What is wrong with the value, shown at the control. */
  error?: React.ReactNode;
  /** One form control. It gets the id, the description and the invalid mark. */
  children: React.ReactElement<{
    id?: string;
    "aria-describedby"?: string;
    "aria-invalid"?: boolean;
  }>;
}

/** A label, one control, an optional hint and an error shown right under the control. */
function Field({ label, hint, error, children, className, ...props }: FieldProps) {
  const id = React.useId();
  const hintId = hint ? `${id}-hint` : undefined;
  const errorId = error ? `${id}-error` : undefined;
  const describedBy = [errorId, hintId].filter(Boolean).join(" ") || undefined;
  return (
    <div data-slot="field" className={cn("grid min-w-0 gap-1.5", className)} {...props}>
      <Label htmlFor={id}>{label}</Label>
      {React.cloneElement(children, {
        id,
        "aria-describedby": describedBy,
        ...(error ? { "aria-invalid": true } : {}),
      })}
      {error ? (
        <p id={errorId} role="alert" className="text-xs leading-snug text-destructive">
          {error}
        </p>
      ) : null}
      {hint ? (
        <p id={hintId} className="text-xs leading-snug text-muted-foreground">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export { Field };
