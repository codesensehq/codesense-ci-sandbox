// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { listing as paneListing } from "@/features/shell/right-pane/panes/pane-seam-helpers";
import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { Note, NoteMeta } from "@/tauri/commands";
import { listNotes, readNote } from "@/tauri/commands";
import { upsertSupportingDoc } from "@/features/dashboard/lib/attach-supporting-doc";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { RelatedDocsChip, type RelateDocActions } from "./related-docs-chip";
import { formatCsHref } from "../lib/note-links";

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, listNotes: vi.fn(), readNote: vi.fn() };
});
vi.mock("@/features/dashboard/lib/attach-supporting-doc", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/features/dashboard/lib/attach-supporting-doc")>();
  return { ...actual, upsertSupportingDoc: vi.fn(actual.upsertSupportingDoc) };
});

const STORY = "/ctx/content/epic-a/SW-1.md";
const CONTENT = "/ctx/content";
const resolveCtx = { nodes: [{ storyId: "SW-1", storyFile: STORY }], teamContentPath: CONTENT };

const listNotesMock = listNotes as Mock;
const readNoteMock = readNote as Mock;
const upsertMock = upsertSupportingDoc as Mock;

function meta(over: Partial<NoteMeta> & { id: string }): NoteMeta {
  return { title: over.id, pinned: false, lastOpenedAt: 0, ...over };
}
function noteBody(id: string, content: string): Note {
  return { ...meta({ id }), content };
}

const SUPPORTING = [
  { fileName: "diagram.md", filePath: "/ctx/content/epic-a/diagram.md", documentType: "doc" },
];

function renderChip(over: Partial<React.ComponentProps<typeof RelatedDocsChip>> = {}) {
  const onOpenFile = vi.fn();
  render(
    <RelatedDocsChip
      activeTabPath={STORY}
      supportingDocs={SUPPORTING}
      docIdentity={{ storyId: "SW-1" }}
      resolveCtx={resolveCtx}
      onOpenFile={onOpenFile}
      {...over}
    />,
  );
  return { onOpenFile };
}

describe("RelatedDocsChip (SW-SN-10)", () => {
  let createNoteSpy: Mock<(title?: string, body?: string) => Promise<string>>;
  let setActiveSpy: Mock<(id: string) => Promise<void>>;
  let revealSpy: Mock<() => void>;

  beforeEach(() => {
    vi.clearAllMocks();
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "dev" });
    usePaneStore.setState({ listing: null, knownEntries: {}, pendingIntents: {} });
    useDashboardProfileStore.setState({ org: "acme", profile: null });
    listNotesMock.mockResolvedValue([]);
    readNoteMock.mockResolvedValue(noteBody("x", ""));
    createNoteSpy = vi
      .fn<(title?: string, body?: string) => Promise<string>>()
      .mockResolvedValue("new-1");
    setActiveSpy = vi.fn<(id: string) => Promise<void>>().mockResolvedValue(undefined);
    revealSpy = vi.fn<() => void>();
    useViewerStore.setState({
      createNote: createNoteSpy,
      setActiveNote: setActiveSpy,
      revealPanelById: revealSpy,
    });
  });

  it("AC-2: supporting-doc row opens in the viewer via onOpenFile", async () => {
    const { onOpenFile } = renderChip();
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(screen.getByText("diagram.md"));
    expect(onOpenFile).toHaveBeenCalledWith("/ctx/content/epic-a/diagram.md");
  });

  it("AC-3: derives note backlinks on-demand — linking note shows, non-linking omitted", async () => {
    listNotesMock.mockResolvedValue([
      meta({ id: "n1", title: "Linked" }),
      meta({ id: "n2", title: "Unrelated" }),
    ]);
    readNoteMock.mockImplementation((_uid: number, id: string) =>
      Promise.resolve(
        id === "n1"
          ? noteBody("n1", `see [SW-1](${formatCsHref({ kind: "story", target: "SW-1" })})`)
          : noteBody("n2", "no links here"),
      ),
    );
    renderChip();
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    expect(await screen.findByText("Linked")).toBeInTheDocument();
    expect(screen.queryByText("Unrelated")).toBeNull();
  });

  it("AC-3: scan runs only on open (not on mount) — non-blocking", async () => {
    renderChip();
    expect(listNotesMock).not.toHaveBeenCalled(); // nothing scanned until opened
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await waitFor(() => expect(listNotesMock).toHaveBeenCalled());
  });

  it("AC-2: note row reveals the pane THEN focuses the note", async () => {
    listNotesMock.mockResolvedValue([meta({ id: "n1", title: "Linked" })]);
    readNoteMock.mockResolvedValue(
      noteBody("n1", `[x](${formatCsHref({ kind: "story", target: "SW-1" })})`),
    );
    renderChip();
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(await screen.findByText("Linked"));
    expect(revealSpy).toHaveBeenCalledOnce();
    expect(setActiveSpy).toHaveBeenCalledWith("n1");
  });

  it("AC-4: 'New note about this' creates a pre-linked note, reveals + focuses it", async () => {
    renderChip();
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(screen.getByText("New note about this"));
    await waitFor(() => expect(createNoteSpy).toHaveBeenCalled());
    const [, body] = createNoteSpy.mock.calls[0];
    expect(body).toContain(formatCsHref({ kind: "story", target: "SW-1" })); // pre-linked
    expect(revealSpy).toHaveBeenCalled();
    expect(setActiveSpy).toHaveBeenCalledWith("new-1");
  });

  it.each([false, true])(
    "routes Related notes to the chosen dashboard (create: %s)",
    async (create) => {
      const panes = paneListing({
        org: "acme",
        team: "dev",
        root: "/ctx",
        id: "notes",
        title: "Notes",
        privateCopy: true,
        surface: "notes",
      });
      usePaneStore.setState({ teamKey: "acme:dev", listing: panes });
      useDashboardProfileStore.setState({
        org: "acme",
        profile: {
          id: "default",
          name: "Default",
          posture: "",
          slots: { "right-pane": [panes.panes[0].dashboardRef!] },
          cliLayout: "default",
          chrome: {},
        },
      });
      listNotesMock.mockResolvedValue([meta({ id: "n1", title: "Linked" })]);
      readNoteMock.mockResolvedValue(
        noteBody("n1", `[x](${formatCsHref({ kind: "story", target: "SW-1" })})`),
      );
      renderChip();
      await userEvent.click(screen.getByRole("button", { name: /related/i }));
      await userEvent.click(await screen.findByText(create ? "New note about this" : "Linked"));
      await waitFor(() => expect(revealSpy).toHaveBeenCalledWith(panes.panes[0].panelId));
      expect(usePaneStore.getState().pendingIntents[panes.panes[0].panelId]).toEqual({
        noteId: create ? "new-1" : "n1",
      });
    },
  );

  it("AC-6: read-only — no interaction ever writes the repo doc (upsertSupportingDoc)", async () => {
    const { onOpenFile } = renderChip();
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(screen.getByText("diagram.md"));
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(screen.getByText("New note about this"));
    await waitFor(() => expect(createNoteSpy).toHaveBeenCalled());
    expect(upsertMock).not.toHaveBeenCalled();
    expect(onOpenFile).toHaveBeenCalled();
  });

  it("AC-4: an unbuildable link leaves the body unset (keeps the template, no clobber)", async () => {
    renderChip({ docIdentity: {} }); // buildCsLinkToDoc returns null
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(screen.getByText("New note about this"));
    await waitFor(() => expect(createNoteSpy).toHaveBeenCalled());
    expect(createNoteSpy.mock.calls[0][1]).toBeUndefined();
  });

  it("AC-1 / Decision #1: with no supporting docs the minimal chip still offers New-note", async () => {
    renderChip({ supportingDocs: [], docIdentity: { filePath: STORY, teamContentPath: CONTENT } });
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    expect(screen.getByText("New note about this")).toBeInTheDocument();
  });

  // ---- SLW-FX-21: relate (cap 1) + unlink (cap 3) ----

  function makeActions(over: Partial<RelateDocActions> = {}): RelateDocActions {
    return {
      candidates: [{ filePath: "/ctx/content/epic-a/other.md", fileName: "other.md" }],
      relate: vi.fn(),
      unlink: vi.fn(),
      ...over,
    };
  }

  it("SLW-FX-21 AC-2: shows 'Relate a file…' even when there are no supporting docs", async () => {
    renderChip({ supportingDocs: [], docActions: makeActions() });
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    expect(screen.getByText(/relate a file/i)).toBeInTheDocument();
  });

  it("SLW-FX-21 AC-2: picking a file in the picker calls docActions.relate with its path", async () => {
    const relate = vi.fn();
    renderChip({ supportingDocs: [], docActions: makeActions({ relate }) });
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(screen.getByText(/relate a file/i));
    // picker overlay open → pick the candidate by its full-path sublabel
    await userEvent.click(screen.getByText("/ctx/content/epic-a/other.md"));
    expect(relate).toHaveBeenCalledWith("/ctx/content/epic-a/other.md");
  });

  it("SLW-FX-21 AC-3: per-row unlink calls docActions.unlink and does NOT open the file", async () => {
    const unlink = vi.fn();
    const { onOpenFile } = renderChip({ docActions: makeActions({ unlink }) });
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    await userEvent.click(screen.getByRole("button", { name: /unlink diagram\.md/i }));
    expect(unlink).toHaveBeenCalledWith("/ctx/content/epic-a/diagram.md");
    expect(onOpenFile).not.toHaveBeenCalled();
  });

  it("SLW-FX-21 AC-8: without docActions (read-only doc) no relate/unlink affordances render", async () => {
    renderChip(); // no docActions → read-only chip
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    expect(screen.queryByText(/relate a file/i)).toBeNull();
    expect(screen.queryByRole("button", { name: /unlink/i })).toBeNull();
  });

  it("SLW-FX-21: the 'My notes' section is never writable (New-note still the only notes action)", async () => {
    renderChip({ docActions: makeActions() });
    await userEvent.click(screen.getByRole("button", { name: /related/i }));
    // relate/unlink live only in the team supporting-docs section; notes stay read-only
    expect(screen.getByText("New note about this")).toBeInTheDocument();
    expect(upsertMock).not.toHaveBeenCalled();
  });
});

it("uses the shared header recipe and keeps its count and accessible name in icon-only mode", async () => {
  const { HEADER_BTN } = await import("./header-button");
  listNotesMock.mockResolvedValue([]);
  renderChip();
  const trigger = screen.getByRole("button", { name: "Related documents and notes, 1" });
  expect(trigger.className).toBe(HEADER_BTN);
  expect(trigger).toHaveAttribute("title", "Related documents and notes, 1");
  expect(trigger).toHaveAttribute("aria-expanded", "false");
  expect(trigger.querySelector("svg + span")).toHaveTextContent("1");
  expect(screen.getByText("Related")).toHaveClass("viewer-header-label");
  await userEvent.click(trigger);
  expect(trigger).toHaveAttribute("aria-expanded", "true");
});
