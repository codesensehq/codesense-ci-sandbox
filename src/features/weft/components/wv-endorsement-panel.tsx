// SPDX-License-Identifier: MIT
import { memo, useMemo } from "react";
import type { WvPendingAssertionDto } from "@/tauri/commands";
import { assertionRowModel, WeftBoardYouOweRow } from "./weft-board-you-owe-row";

export interface WvEndorsementPanelProps {
  assertions?: WvPendingAssertionDto[];
  error?: string;
}
const settled = () => {};

/** Rows are authoritative hub state; successful actions arrive through the same writer seam. */
export const WvEndorsementPanel = memo(function WvEndorsementPanel({
  assertions,
  error,
}: WvEndorsementPanelProps): React.JSX.Element {
  const rows = useMemo(
    () => assertions?.map((assertion) => assertionRowModel(assertion)),
    [assertions],
  );
  if (error != null) {
    return (
      <div className="p-3 text-xs text-destructive" role="alert">
        Couldn&apos;t load endorsements: {error}
      </div>
    );
  }

  if (rows == null) {
    return (
      <div className="p-3 text-xs text-muted-foreground" data-testid="endorsement-loading">
        Loading…
      </div>
    );
  }

  if (rows.length === 0) {
    return (
      <div
        className="p-6 text-center text-sm text-muted-foreground"
        data-testid="endorsement-empty"
      >
        All endorsed.
      </div>
    );
  }

  return (
    <div data-testid="endorsement-panel" className="flex flex-col">
      {rows.map((row) => (
        <WeftBoardYouOweRow key={row.id} row={row} onRatified={settled} onRevoked={settled} />
      ))}
    </div>
  );
});
