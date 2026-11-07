// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";
import userEvent from "@testing-library/user-event";

import { WeftFindingsView } from "@/features/viewer/components/weft-findings-view";
import type { PerNodeResult } from "@/shared/lib/run-evidence";

describe("WeftFindingsView (SW-WV-F4 AC-3/AC-4)", () => {
  it("renders one row per summarised node; only the node with a file artifact has a diff chevron (AC-3)", async () => {
    const user = userEvent.setup();
    const onOpenDiff = vi.fn<(path: string) => void>();
    const results: PerNodeResult[] = [
      {
        nodeId: "review",
        summary: "Refactored the parser",
        artifacts: [{ kind: "file", reference: "src/parser.ts" }],
      },
      { nodeId: "notes", summary: "Left a note", artifacts: [] },
    ];

    render(<WeftFindingsView perNodeResults={results} onOpenDiff={onOpenDiff} />);

    expect(screen.getAllByTestId("findings-row")).toHaveLength(2);
    const chevrons = screen.getAllByTestId("findings-diff-chevron");
    expect(chevrons).toHaveLength(1);

    await user.click(chevrons[0]);
    expect(onOpenDiff).toHaveBeenCalledWith("src/parser.ts");
  });

  it("skips nodes with a blank summary and shows the empty state when nothing remains (AC-3)", () => {
    const onOpenDiff = vi.fn<(path: string) => void>();
    render(
      <WeftFindingsView
        perNodeResults={[{ nodeId: "n1", summary: "   " }, { nodeId: "n2" }]}
        onOpenDiff={onOpenDiff}
      />,
    );
    expect(screen.queryByTestId("findings-row")).not.toBeInTheDocument();
    expect(screen.getByTestId("findings-empty")).toHaveTextContent("No findings recorded");
  });

  it("shows the empty state for an empty perNodeResults (AC-3)", () => {
    render(<WeftFindingsView perNodeResults={[]} onOpenDiff={vi.fn()} />);
    expect(screen.getByTestId("findings-empty")).toBeInTheDocument();
  });

  it("flags a summary token absent from the artifacts (AC-4) and suppresses it when the ref matches", () => {
    const { rerender } = render(
      <WeftFindingsView
        perNodeResults={[
          {
            nodeId: "n1",
            summary: "touched src/foo.ts",
            artifacts: [{ kind: "pr", reference: "https://x/1" }],
          },
        ]}
        onOpenDiff={vi.fn()}
      />,
    );
    // The summary names `src/foo.ts` but no artifact reference contains it → flagged.
    expect(screen.getByTestId("findings-mismatch-badge")).toBeInTheDocument();

    rerender(
      <WeftFindingsView
        perNodeResults={[
          {
            nodeId: "n1",
            summary: "touched src/foo.ts",
            artifacts: [{ kind: "file", reference: "src/foo.ts" }],
          },
        ]}
        onOpenDiff={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("findings-mismatch-badge")).not.toBeInTheDocument();
  });

  it("never false-alarms the badge when the node produced no artifacts (AC-4)", () => {
    render(
      <WeftFindingsView
        perNodeResults={[{ nodeId: "n1", summary: "touched src/foo.ts", artifacts: [] }]}
        onOpenDiff={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("findings-mismatch-badge")).not.toBeInTheDocument();
  });
});
