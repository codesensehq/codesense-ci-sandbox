// SPDX-License-Identifier: MIT
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";

import { ViewerToolbar } from "../components/viewer-toolbar";
import { activeFileScope, docToolbarGates } from "./doc-toolbar-gates";
import { isReadOnlyLayeredDoc } from "./is-read-only-layered-doc";

vi.mock("../components/related-docs-chip", () => ({
  RelatedDocsChip: () => <div data-testid="related-chip" />,
}));
vi.mock("../components/metadata-chip", () => ({
  MetadataChip: () => <div data-testid="metadata-chip" />,
}));

// SW-XS-20 (AC-1): opening a file inherited from a parent team in a child team's
// viewer. It stays editable and says where edits go: the file's path IS the
// parent's file, so the editor, the metadata band and relate all write there.

const PARENT_FILE = "/org/platform/content/rules/rule.md";
const OWN_FILE = "/org/default/content/rules/mine.md";
const navTree = buildTree("root", {
  absoluteDirPath: "/org/default/content",
  subEpics: [
    buildTree("rules", {
      dirPath: "rules",
      absoluteDirPath: "/org/default/content/rules",
      docs: [
        { fileName: "rule.md", filePath: PARENT_FILE, sourceTeam: "platform" },
        { fileName: "mine.md", filePath: OWN_FILE },
      ],
    }),
  ],
});

function renderFor(filePath: string) {
  const scope = activeFileScope([], navTree, filePath, "default");
  const gates = docToolbarGates(filePath, scope, false);
  render(
    <ViewerToolbar
      activeTabPath={filePath}
      relatedDoc={null}
      relateDocActions={undefined}
      inheritedActive={gates.inheritedActive}
      activeScope={scope}
      copyIntoTeamName="default"
      onCopyIntoTeam={vi.fn()}
      showMetadataChip={gates.showMetadataChip}
      showReadOnlyChip={gates.canEditActive}
      canEditActive={gates.canEditActive}
      editingActive={false}
      editDirty={false}
      onEditActive={vi.fn()}
      metadataOpen={false}
      toggleMetadata={vi.fn()}
      metadataValid
      metadataChipRef={{ current: null }}
      regionId="metadata-band-region"
      onOpenFile={vi.fn()}
    />,
  );
  return { scope, gates };
}

describe("viewer on a file inherited from a parent team (SW-XS-20 AC-1)", () => {
  afterEach(cleanup);

  it("labels it, keeps Edit and the Metadata chip, and offers Copy into the team", () => {
    const { scope, gates } = renderFor(PARENT_FILE);
    expect(scope).toEqual({ repoSlug: null, sourceTeam: "platform", isInherited: true });
    expect(gates).toEqual({ inheritedActive: true, showMetadataChip: true, canEditActive: true });
    expect(screen.getByTestId("inherited-file-explainer")).toHaveTextContent(
      "This file comes from platform. Changes you make here update it for platform and every team that inherits from it.",
    );
    expect(screen.getByLabelText("Read-only")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit" })).toBeInTheDocument();
    expect(screen.getByTestId("metadata-chip")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Copy into default/ })).toBeInTheDocument();
  });

  it("keeps relate actions (only source-repo files are read-only)", () => {
    expect(isReadOnlyLayeredDoc(navTree, PARENT_FILE)).toBe(false);
  });

  it("saves to the parent's file: the open path is the parent's real path", () => {
    // The editor inset, metadata band and relate write to the open tab's path.
    expect(PARENT_FILE.startsWith("/org/platform/content/")).toBe(true);
    expect(activeFileScope([], navTree, PARENT_FILE, "default")?.sourceTeam).toBe("platform");
  });

  it("shows no label on the team's own file", () => {
    const { gates } = renderFor(OWN_FILE);
    expect(gates.inheritedActive).toBe(false);
    expect(screen.queryByTestId("inherited-file-explainer")).toBeNull();
    expect(screen.queryByRole("button", { name: /Copy into/ })).toBeNull();
  });
});
