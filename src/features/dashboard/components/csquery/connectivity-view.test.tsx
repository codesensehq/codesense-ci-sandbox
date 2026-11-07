// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ConnectivityView } from "./connectivity-view";
import type { DocSummary, RelRef } from "@/shared/lib/doc-query/types";

const A = "/ctx/epic-a";

function ref(path: string, resolved = true): RelRef {
  const fileName = path.split("/").pop()!;
  return { id: fileName, path, fileName, resolved };
}
function doc(
  path: string,
  o: { status?: string | null; forward?: RelRef[]; backlinks?: RelRef[] } = {},
): DocSummary {
  const fileName = path.split("/").pop()!;
  return {
    id: fileName,
    path,
    fileName,
    tags: {},
    status: o.status ?? null,
    documentType: "story",
    mtime: null,
    sourceRepo: null,
    sprint: null,
    points: null,
    assignedTo: null,
    relationships: {
      supportingDocs: o.forward ?? [],
      backlinks: o.backlinks ?? [],
      dependsOn: [],
    },
  };
}

function renderView(
  docs: DocSummary[],
  extra: Partial<React.ComponentProps<typeof ConnectivityView>> = {},
) {
  return render(
    <ConnectivityView docs={docs} scopeRoot={A} scopeLabel="epic-a" scopeFound {...extra} />,
  );
}

describe("ConnectivityView", () => {
  it("renders a row per story with a health badge and navigable chips", () => {
    const onNavigate = vi.fn();
    renderView(
      [doc(`${A}/a1.md`, { status: "todo", forward: [ref(`${A}/arch.md`), ref(`${A}/x.md`)] })],
      { onNavigate },
    );
    expect(screen.getByText("a1.md")).toBeInTheDocument();
    expect(screen.getByText("Healthy")).toBeInTheDocument();
    // chip click navigates to the target file
    fireEvent.click(screen.getByText("arch.md"));
    expect(onNavigate).toHaveBeenCalledWith(`${A}/arch.md`);
    // story click navigates to the story
    fireEvent.click(screen.getByText("a1.md"));
    expect(onNavigate).toHaveBeenCalledWith(`${A}/a1.md`);
  });

  it("flags a context-desert story as 'will be built blind'", () => {
    renderView([doc(`${A}/blind.md`)]);
    expect(screen.getByText("Will be built blind")).toBeInTheDocument();
  });

  it("shows the all-desert banner when every story is blind", () => {
    renderView([doc(`${A}/a.md`), doc(`${A}/b.md`)]);
    expect(screen.getByText(/no context wired up yet/i)).toBeInTheDocument();
  });

  it("omits the orphan section when there are no orphans, shows it otherwise", () => {
    // linked pair → no orphans
    const { rerender } = renderView([
      doc(`${A}/linker.md`, { forward: [ref(`${A}/linked.md`)] }),
      doc(`${A}/linked.md`, { backlinks: [ref(`${A}/linker.md`)] }),
    ]);
    expect(screen.queryByText(/Orphan docs/i)).not.toBeInTheDocument();

    rerender(
      <ConnectivityView
        docs={[
          doc(`${A}/island.md`),
          doc(`${A}/l.md`, { forward: [ref(`${A}/m.md`)] }),
          doc(`${A}/m.md`, { backlinks: [ref(`${A}/l.md`)] }),
        ]}
        scopeRoot={A}
        scopeLabel="epic-a"
        scopeFound
      />,
    );
    expect(screen.getByText(/Orphan docs/i)).toBeInTheDocument();
  });

  it("renders empty states without crashing (AC-12)", () => {
    const { rerender } = render(
      <ConnectivityView docs={[]} scopeRoot={null} scopeLabel={null} scopeFound={false} />,
    );
    expect(screen.getByText(/needs a `scope:`/i)).toBeInTheDocument();

    rerender(<ConnectivityView docs={[]} scopeRoot={A} scopeLabel="nope" scopeFound={false} />);
    expect(screen.getByText(/Scope not found: nope/i)).toBeInTheDocument();

    rerender(<ConnectivityView docs={[]} scopeRoot={A} scopeLabel="epic-a" scopeFound />);
    expect(screen.getByText(/No stories in this scope/i)).toBeInTheDocument();
  });

  it("toggles to the story×type matrix", () => {
    renderView([doc(`${A}/a1.md`, { forward: [ref(`${A}/arch.md`)] })]);
    fireEvent.click(screen.getByText("Matrix"));
    expect(screen.getByText("List")).toBeInTheDocument(); // toggle flipped
    expect(screen.getByText("Story")).toBeInTheDocument(); // matrix header
  });
});
