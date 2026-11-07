// SPDX-License-Identifier: MIT
import { openRoleEditor } from "@/features/agents/stores/role-editor-store";
import { QueueRole } from "@/shared/components/queue-role";
import type { RoleHolding } from "../hooks/use-role-holdings";

export function SessionRoleChips({ holds }: { holds: readonly RoleHolding[] }) {
  if (!holds.length) return null;
  return (
    <span
      className="ag-holds mt-6 mr-2 min-w-0 max-w-[65%] shrink-0 self-start truncate"
      data-testid="agent-holds"
    >
      {holds.slice(0, 2).map((h) => (
        <button
          key={`${h.teamKey}/${h.role}`}
          type="button"
          className="queue-role-chip"
          onKeyDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            void openRoleEditor(h.teamKey, h.role);
          }}
        >
          <QueueRole role={h.role} teamKey={h.teamKey} />
        </button>
      ))}
      {holds.length > 2 && (
        <span title={holds.map((h) => h.label).join(", ")}>+{holds.length - 2}</span>
      )}
    </span>
  );
}
