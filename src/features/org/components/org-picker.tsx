// SPDX-License-Identifier: MIT
import { useLocalSetupActivity } from "../lib/local-setup-completion";
import { ContextAttachments } from "./context-attachments";
interface OrgPickerProps {
  canDismiss: boolean;
  onDismiss?: () => void;
  initialScreen?: "home" | "create" | "join";
  embedded?: boolean;
}
/** Connect your team's context repo, or create a new one. */
export function OrgPicker({
  canDismiss,
  onDismiss,
  initialScreen = "home",
  embedded = false,
}: OrgPickerProps) {
  const busy = useLocalSetupActivity((s) => s.busy);
  return (
    <section className={embedded ? "space-y-4 p-4" : "mx-auto max-w-xl space-y-4 p-8"}>
      <h1 className="text-lg font-semibold">Context repos</h1>
      <p className="text-sm text-muted-foreground">
        A context repo holds your team's instructions, skills, plans and dashboards. Connect the one
        your team cloned, or create a new one.
      </p>
      <ContextAttachments initialCreate={initialScreen === "create"} onAttached={onDismiss} />
      {canDismiss && (
        <button type="button" disabled={busy} onClick={onDismiss}>
          Done
        </button>
      )}
    </section>
  );
}
