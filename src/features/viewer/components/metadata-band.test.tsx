// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, waitForElementToBeRemoved, act } from "@/test-utils";

import type { MetadataModel } from "@/tauri/commands";

// ---- command mocks --------------------------------------------------------
const getMetadataModel = vi.fn();
const applyMetadataEdit = vi.fn();
const canDeclareTeamField = vi.fn().mockResolvedValue({ allowed: true });
const declareTeamField = vi.fn();
const validateStoryFile = vi.fn();
vi.mock("@/tauri/commands", () => ({
  validateStoryFile: (...a: unknown[]) => validateStoryFile(...a),
  getMetadataModel: (...a: unknown[]) => getMetadataModel(...a),
  applyMetadataEdit: (...a: unknown[]) => applyMetadataEdit(...a),
  // SW-AU-77E: the drawer pre-flights the declare gate on mount.
  canDeclareTeamField: (...a: unknown[]) => canDeclareTeamField(...a),
  declareTeamField: (...a: unknown[]) => declareTeamField(...a),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { warn: vi.fn(), error: vi.fn(), info: vi.fn(), debug: vi.fn() },
}));

// SW-ID-5: the assignee picker reads the roster itself — give it a fixed one.
vi.mock("@/shared/stores/use-people-by-login", async () => {
  const { buildPeopleByLogin } = await import("@/shared/lib/people-by-login");
  return {
    usePeopleByLogin: () =>
      buildPeopleByLogin([
        {
          github_user_id: 2,
          github_username: "octocat",
          display_name: "Octo Cat",
          avatar_url: null,
        },
      ]),
  };
});

// SW-WB-1: the band reconcile handler dynamic-imports the viewer store. Mock it
// so the band test stays light and can assert the reconcile call.
const reconcileWorktreeBranch = vi.fn().mockResolvedValue({ path: "/wt/new", mode: "recreate" });
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: { getState: () => ({ reconcileWorktreeBranch }) },
}));

// SW-WB-1: capture toasts so we can assert the AC-7 recreate-discards action.
const toastError = vi.fn();
const toastSuccess = vi.fn();
vi.mock("sonner", () => ({
  toast: {
    error: (...a: unknown[]) => toastError(...a),
    success: (...a: unknown[]) => toastSuccess(...a),
  },
}));

import { MetadataBand } from "./metadata-band";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";

const FILE = "/root/s.md";
const REPO = "/root";

function model(over: Partial<MetadataModel> = {}): MetadataModel {
  return {
    format: "markdown",
    fields: { type: "story", status: "in-progress" },
    unknownFields: [],
    isEpic: false,
    blockParseOk: true,
    perFieldValidity: [],
    schemaRows: [],
    declarableFields: [],
    rawBlock: null,
    ...over,
  };
}

function renderBand(readOnly = false) {
  return render(
    <MetadataBand
      filePath={FILE}
      repoPath={REPO}
      orgRoot="/org"
      orgSlug="acme"
      team="default"
      sourceRepoOptions={[]}
      knownTeams={["default"]}
      readOnly={readOnly}
      onClose={vi.fn()}
      regionId="region"
    />,
  );
}

// Render and wait for the initial async metadata load to finish. The band's
// header renders before `getMetadataModel` resolves, so waiting on the header
// races the field mount; wait for the "Loading metadata…" state to clear
// instead. Use this when asserting absence or post-load side effects; positive
// element checks can just `await screen.findBy*` the element directly.
//
// Constraint: valid only at initial mount with a resolving `getMetadataModel`
// mock. The band shows "Loading metadata…" only on the first load — a re-load
// keeps the prior `model`, so the text never reappears — and a rejecting mock
// skips the loading→loaded transition. In either case `waitForElementToBeRemoved`
// throws, so never call this after a refresh bump or with a rejected model.
async function renderBandLoaded(readOnly = false) {
  const utils = renderBand(readOnly);
  await waitForElementToBeRemoved(() => screen.queryByText(/Loading metadata/i));
  return utils;
}

beforeEach(() => {
  validateStoryFile.mockReset();
  validateStoryFile.mockResolvedValue({
    detectedKind: "pipeline_story",
    format: "markdown",
    valid: true,
    findings: [],
  });
  getMetadataModel.mockReset();
  applyMetadataEdit.mockReset();
  applyMetadataEdit.mockResolvedValue(undefined);
  useFileRefreshStore.setState({
    fileRefreshKeys: {},
    lastChangedContextFile: {},
    contentRefreshKeys: {},
  });
});

describe("MetadataBand", () => {
  it("auto-saves a field commit and bumps the refresh store (Task 7.1)", async () => {
    getMetadataModel.mockResolvedValue(model());
    renderBand();

    // Commit a scalar change via the status combobox: focus, type, Enter.
    const statusInput = await screen.findByLabelText("status");
    fireEvent.focus(statusInput);
    fireEvent.change(statusInput, { target: { value: "qa" } });
    fireEvent.keyDown(statusInput, { key: "Enter" });

    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "setField", field: "status", value: "qa" },
      ]),
    );
    // The refresh store was bumped for this repo + file.
    await waitFor(() => {
      const changed = useFileRefreshStore.getState().lastChangedContextFile[REPO];
      expect(changed?.has(FILE)).toBe(true);
    });
  });

  it("SW-AU-77E: declares a typed field from the drawer and sets its value in one action", async () => {
    getMetadataModel.mockResolvedValue(model());
    declareTeamField.mockResolvedValue({
      fields: [{ name: "risk", type: "text", required: false, official: false }],
      warnings: [],
    });
    await renderBandLoaded();

    // Open the "+ Add field" menu, then the typed "New field…" form.
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByTestId("metadata-add-custom-field"));

    fireEvent.change(await screen.findByLabelText("new field name"), {
      target: { value: "risk" },
    });
    fireEvent.change(screen.getByLabelText("new field value"), {
      target: { value: "high" },
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("declare-field-submit"));
    });

    // AC-6: one action declares the team field AND sets its value on this story.
    expect(declareTeamField).toHaveBeenCalledWith(
      "/org",
      "default",
      expect.objectContaining({ name: "risk", type: "text", value: "high" }),
    );
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "setField", field: "risk", value: "high" },
      ]),
    );
  });

  it("SW-AU-77E: a set-failure after a successful declare surfaces the error, never silent (AC-6)", async () => {
    getMetadataModel.mockResolvedValue(model());
    declareTeamField.mockResolvedValue({
      fields: [{ name: "risk", type: "text", required: false, official: false }],
      warnings: [],
    });
    // The declare succeeds but the value write fails.
    applyMetadataEdit.mockRejectedValueOnce("disk full");
    await renderBandLoaded();

    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByTestId("metadata-add-custom-field"));
    fireEvent.change(await screen.findByLabelText("new field name"), {
      target: { value: "risk" },
    });
    fireEvent.change(screen.getByLabelText("new field value"), {
      target: { value: "high" },
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("declare-field-submit"));
    });

    // The field was declared, but the set-failure surfaces in the form (not a silent
    // half-success), and the self-write guard was reset so a later external change isn't lost.
    expect(await screen.findByRole("alert")).toHaveTextContent("disk full");
  });

  it.each([FILE, REPO])(
    "shows the Reload banner without clobber for %s mid-edit (AC-14)",
    async (changed) => {
      getMetadataModel.mockResolvedValue(model());
      renderBand();

      // Enter an edit-in-flight state: focus a field (onFocusCapture sets editing).
      fireEvent.focus(await screen.findByLabelText("status"));

      // Simulate an external change to THIS file.
      act(() => {
        useFileRefreshStore.getState().bumpFileRefresh(REPO, new Set([changed]));
      });

      // Passive banner appears; the model was NOT silently re-read (getMetadataModel
      // still only called once, for the initial load).
      await screen.findByText(/File changed on disk/i);
      expect(getMetadataModel).toHaveBeenCalledTimes(1);
      expect(screen.getByRole("button", { name: /Reload/i })).toBeInTheDocument();
    },
  );

  it.each([FILE, REPO])(
    "silently re-reads for %s while no field is mid-edit (AC-14)",
    async (changed) => {
      getMetadataModel.mockResolvedValue(model());
      await renderBandLoaded();
      expect(getMetadataModel).toHaveBeenCalledTimes(1);

      // No focus → not editing. External change re-reads silently, no banner.
      act(() => {
        useFileRefreshStore.getState().bumpFileRefresh(REPO, new Set([changed]));
      });
      await waitFor(() => expect(getMetadataModel).toHaveBeenCalledTimes(2));
      expect(screen.queryByText(/File changed on disk/i)).not.toBeInTheDocument();
    },
  );

  it("SR-9: a counter-only ContentLayerEvent storm does NOT re-read the open file", async () => {
    getMetadataModel.mockResolvedValue(model());
    await renderBandLoaded();
    expect(getMetadataModel).toHaveBeenCalledTimes(1);

    // A ContentLayerEvent materialize storm counter-only bumps `fileRefreshKeys`
    // (leaving `lastChangedContextFile` at its resting null) ~1×/sec. The band now
    // keys on `contentRefreshKeys`, which a counter-only bump never advances, so it
    // must NOT re-read — no matter how many waves fire.
    act(() => {
      for (let i = 0; i < 5; i++) {
        useFileRefreshStore.getState().bumpFileRefresh(REPO, undefined, true);
      }
    });

    // Scan counter advanced 5×, but the band did not touch disk.
    expect(useFileRefreshStore.getState().fileRefreshKeys[REPO]).toBe(5);
    expect(getMetadataModel).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(/File changed on disk/i)).not.toBeInTheDocument();
  });

  it("labels the metadata format from the loaded model (html → HTML meta tags)", async () => {
    // Drive the non-default branch so the assertion actually requires the model
    // to load: the header defaults to "markdown frontmatter" pre-load, so only
    // "HTML meta tags" proves the label is derived from the resolved model.
    getMetadataModel.mockResolvedValue(model({ format: "html" }));
    await renderBandLoaded();
    expect(screen.getByText(/HTML meta tags/i)).toBeInTheDocument();
    // The label is an exclusive branch — the markdown default must not co-render.
    expect(screen.queryByText(/markdown frontmatter/i)).not.toBeInTheDocument();
  });

  it("shows a loading state until the async metadata model resolves", async () => {
    // Pins the invariant renderBandLoaded() depends on: the band renders a
    // "Loading metadata…" state synchronously on first commit (before the async
    // model resolves), so waitForElementToBeRemoved always has a node to await.
    getMetadataModel.mockResolvedValue(model());
    renderBand();
    expect(screen.getByText(/Loading metadata/i)).toBeInTheDocument();
    await waitForElementToBeRemoved(() => screen.queryByText(/Loading metadata/i));
  });

  it("disables controls + shows the read-only note in read-only mode (AC-13)", async () => {
    getMetadataModel.mockResolvedValue(model());
    renderBand(true);
    await screen.findByText(/read-only/i);
    // Status control is disabled (await the field — it mounts after the async model).
    expect(await screen.findByLabelText("status")).toBeDisabled();
    // No Add field affordance in read-only mode. Safe as an absence check: the
    // awaited status field above proves the form mounted, so its absence is real.
    expect(screen.queryByRole("button", { name: /Add field/i })).not.toBeInTheDocument();
  });

  it("falls back to the raw editor on a malformed block (AC-12)", async () => {
    // The raw editor seeds from the verbatim on-disk block (`rawBlock`), NOT
    // the best-effort unknownFields pairs — so the user repairs the real text.
    getMetadataModel.mockResolvedValue(
      model({
        blockParseOk: false,
        rawBlock: "status: [broken",
        unknownFields: [["depends_on", "[a, b"]],
      }),
    );
    renderBand();
    await screen.findByText(/can.t be parsed/i);
    const textarea = screen.getByLabelText("Raw metadata block") as HTMLTextAreaElement;
    expect(textarea).toBeInTheDocument();
    expect(textarea.value).toBe("status: [broken");

    // Applying routes through replaceRawBlock with the (verbatim-seeded) text.
    fireEvent.click(screen.getByRole("button", { name: /^Apply$/i }));
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "replaceRawBlock", text: "status: [broken" },
      ]),
    );
  });

  it("surfaces the Assignee picker and commits a selection (SW-ID-5 AC-3)", async () => {
    applyMetadataEdit.mockResolvedValue(undefined);
    getMetadataModel.mockResolvedValue(
      model({ fields: { type: "story", status: "in-progress", assigned_to: "octocat" } }),
    );
    renderBand(false);

    // The picker trigger renders under the assigned_to label.
    const trigger = await screen.findByRole("button", { name: "assigned_to" });
    expect(trigger).toBeInTheDocument();

    // Reassigning to Unassigned commits a ClearField through the shared seam.
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("button", { name: "Unassigned" }));
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "clearField", field: "assigned_to" },
      ]),
    );
  });

  it("does NOT offer Assignee in '+ Add field' on an epic doc (SW-ID-5 — story-only)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ isEpic: true, fields: { type: "story", status: "in-progress" } }),
    );
    renderBand(false);
    // Await the loaded editable form via its Add field affordance — the header
    // renders before the async model resolves, so gate the absence checks on load.
    const addField = await screen.findByRole("button", { name: /Add field/i });
    // With no assignee present, the control isn't rendered...
    expect(screen.queryByRole("button", { name: "assigned_to" })).not.toBeInTheDocument();
    // ...and the "+ Add field" menu does not offer it on an epic.
    fireEvent.click(addField);
    expect(screen.queryByRole("button", { name: "assigned_to" })).not.toBeInTheDocument();
  });

  it("SW-XS-5: never offers the retired scope_context / scope_install on an epic", async () => {
    getMetadataModel.mockResolvedValue(model({ isEpic: true, fields: { status: "draft" } }));
    renderBand(false);
    fireEvent.click(await screen.findByRole("button", { name: /Add field/i }));
    expect(screen.getByText("repos")).toBeInTheDocument();
    expect(screen.queryByText("scope_context")).not.toBeInTheDocument();
    expect(screen.queryByText("scope_install")).not.toBeInTheDocument();
  });

  it("SW-XS-5: still shows an existing scope_context on an epic doc", async () => {
    getMetadataModel.mockResolvedValue(
      model({ isEpic: true, fields: { status: "draft", scope_context: ["docs"] } }),
    );
    renderBand(false);
    expect(await screen.findByText("scope_context")).toBeInTheDocument();
  });

  it("still renders a pre-existing assignee on an epic so it can be cleared (not stranded)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        isEpic: true,
        fields: { type: "story", status: "in-progress", assigned_to: "octocat" },
      }),
    );
    renderBand(false);
    // A stray assigned_to on an epic stays visible + removable (never hidden/unremovable).
    // Await the field row directly — the header renders before the async model
    // resolves, so a synchronous query can race the "Loading metadata…" state.
    expect(await screen.findByRole("button", { name: "assigned_to" })).toBeInTheDocument();
  });

  it("shows existing done metadata as an open field without a completed-date control", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        fields: { type: "story", status: "done" },
        unknownFields: [["done", "completed_at: 2026-07-15"]],
      }),
    );
    renderBand();
    expect(await screen.findByLabelText("status")).toBeInTheDocument();
    expect(screen.queryByLabelText("done.completed_at")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /unknown fields/ }));
    expect(screen.getByText("completed_at: 2026-07-15")).toBeInTheDocument();
  });

  it("surfaces dev_branch as a READ-ONLY text field when present (SW-WB-1 AC-2)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        fields: {
          type: "story",
          status: "in-progress",
          dev_branch: "feat/SW-GS-35-story-dev-branch-frontmatter",
        },
      }),
    );
    // Even with the band editable (readOnly=false), dev_branch stays disabled:
    // it is the OBSERVED mirror, never user-editable (edit-trap fix).
    renderBand(false);
    const input = (await screen.findByLabelText("dev_branch")) as HTMLInputElement;
    expect(input).toBeInTheDocument();
    expect(input.value).toBe("feat/SW-GS-35-story-dev-branch-frontmatter");
    expect(input).toBeDisabled();
  });

  it("hides dev_branch when absent (SW-GS-35 AC-6/AC-7)", async () => {
    getMetadataModel.mockResolvedValue(model({ fields: { type: "story", status: "in-progress" } }));
    await renderBandLoaded();
    expect(screen.queryByLabelText("dev_branch")).not.toBeInTheDocument();
  });

  it("surfaces branch_name as an EDITABLE text field, committing edits (SW-WB-1 AC-1)", async () => {
    applyMetadataEdit.mockResolvedValue(undefined);
    getMetadataModel.mockResolvedValue(
      model({
        fields: {
          type: "story",
          status: "in-progress",
          branch_name: "feat/intended",
        },
      }),
    );
    renderBand(false);
    const input = (await screen.findByLabelText("branch_name")) as HTMLInputElement;
    expect(input).toBeInTheDocument();
    expect(input).not.toBeDisabled();
    expect(input.value).toBe("feat/intended");

    // Editing it drives a real metadata write (unlike the inert dev_branch).
    fireEvent.change(input, { target: { value: "fix/corrected" } });
    fireEvent.blur(input);
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(
        FILE,
        expect.arrayContaining([
          expect.objectContaining({
            kind: "setField",
            field: "branch_name",
            value: "fix/corrected",
          }),
        ]),
      ),
    );
  });

  it("hides branch_name when absent (SW-WB-1 AC-4 — additive, back-compat)", async () => {
    getMetadataModel.mockResolvedValue(model({ fields: { type: "story", status: "in-progress" } }));
    await renderBandLoaded();
    expect(screen.queryByLabelText("branch_name")).not.toBeInTheDocument();
  });

  it("shows the mismatch chip when branch_name differs from dev_branch (SW-WB-1 AC-9)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        fields: {
          type: "story",
          status: "in-progress",
          branch_name: "feat/intended",
          dev_branch: "feat/observed",
        },
      }),
    );
    renderBand();
    const chip = await screen.findByLabelText("branch mismatch");
    expect(chip).toHaveTextContent("feat/observed");
    expect(chip).toHaveTextContent("feat/intended");
  });

  it("hides the mismatch chip when branch_name equals dev_branch (SW-WB-1 AC-9)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        fields: {
          type: "story",
          status: "in-progress",
          branch_name: "feat/same",
          dev_branch: "feat/same",
        },
      }),
    );
    await renderBandLoaded();
    expect(screen.queryByLabelText("branch mismatch")).not.toBeInTheDocument();
  });

  it("suppresses the mismatch chip once canonical `branch` is present (SW-OR-1b)", async () => {
    // The intended/observed split is retired under the unified `branch`: even with
    // diverging legacy fields, a present `branch` means there is no two-field
    // mismatch to show (this is also how the legacy reconcile clears the chip).
    getMetadataModel.mockResolvedValue(
      model({
        fields: {
          type: "story",
          status: "in-progress",
          branch: "feat/canonical",
          branch_name: "feat/intended",
          dev_branch: "feat/observed",
        },
      }),
    );
    await renderBandLoaded();
    expect(screen.queryByLabelText("branch mismatch")).not.toBeInTheDocument();
  });

  it("hides the mismatch chip when there is no worktree yet (dev_branch absent)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ fields: { type: "story", status: "in-progress", branch_name: "feat/intended" } }),
    );
    await renderBandLoaded();
    expect(screen.queryByLabelText("branch mismatch")).not.toBeInTheDocument();
  });

  it("clicking Reconcile invokes the store reconcile action (SW-WB-1 AC-5)", async () => {
    reconcileWorktreeBranch.mockClear();
    getMetadataModel.mockResolvedValue(
      model({
        fields: {
          type: "story",
          status: "in-progress",
          branch_name: "feat/intended",
          dev_branch: "feat/observed",
        },
      }),
    );
    renderBand(false);
    await screen.findByLabelText("branch mismatch");
    fireEvent.click(screen.getByRole("button", { name: /Reconcile/i }));
    await waitFor(() =>
      expect(reconcileWorktreeBranch).toHaveBeenCalledWith("acme:default", FILE, undefined),
    );
  });

  it("offers Recreate (discards) when a dirty migrate is refused (SW-WB-1 AC-7)", async () => {
    toastError.mockClear();
    reconcileWorktreeBranch.mockReset();
    reconcileWorktreeBranch
      .mockRejectedValueOnce(new Error("unavailable: the switch was refused"))
      .mockResolvedValueOnce({ path: "/wt/new", mode: "recreate" });
    getMetadataModel.mockResolvedValue(
      model({
        fields: {
          type: "story",
          status: "in-progress",
          branch_name: "feat/intended",
          dev_branch: "feat/observed",
        },
      }),
    );
    renderBand(false);
    await screen.findByLabelText("branch mismatch");
    fireEvent.click(screen.getByRole("button", { name: /Reconcile/i }));

    // The refused migrate surfaces a typed error toast carrying a
    // recreate-that-discards action (never auto-discards).
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    const errOpts = toastError.mock.calls[0][1] as {
      action: { label: string; onClick: () => void };
    };
    expect(errOpts.action.label).toMatch(/discards/i);

    // Invoking it forces the recreate path.
    errOpts.action.onClick();
    await waitFor(() =>
      expect(reconcileWorktreeBranch).toHaveBeenCalledWith("acme:default", FILE, {
        forceRecreate: true,
      }),
    );
  });
});

describe("MetadataBand — declared custom fields (SW-AU-77G)", () => {
  it("renders a present declared choice field as a typed row with the live invalid flag", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "risk",
            values: ["cosmic"],
            type: "choice",
            allowedValues: ["low", "high"],
            source: "Team",
          },
        ],
        perFieldValidity: [
          {
            field: "risk",
            state: "invalid",
            message: "risk value 'cosmic' is not an allowed value",
            code: "INVALID_TAG_VALUE",
          },
        ],
      }),
    );
    renderBand(false);
    const row = await screen.findByRole("button", { name: "risk" });
    expect(row).toBeInTheDocument();
    // The short inline flag surfaces the reason; the full message rides `title`.
    expect(row).toHaveTextContent("not allowed");
    expect(row).toHaveAttribute("aria-invalid", "true");
    expect(row).toHaveAttribute("title", "risk value 'cosmic' is not an allowed value");
    // SW-AU-77I: the reason is ALSO screen-reader available (not title-only).
    expect(row).toHaveAttribute("aria-describedby");
  });

  it("renders a valid declared field with no flag", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "risk",
            values: ["low"],
            type: "choice",
            allowedValues: ["low", "high"],
            source: "Team",
          },
        ],
        perFieldValidity: [{ field: "risk", state: "valid", message: null, code: null }],
      }),
    );
    renderBand(false);
    const row = await screen.findByRole("button", { name: "risk" });
    expect(row).not.toHaveAttribute("aria-invalid");
    expect(row).not.toHaveTextContent("not allowed");
  });

  it("commits an edited declared field via setField with its own key (AC-1)", async () => {
    applyMetadataEdit.mockResolvedValue(undefined);
    getMetadataModel
      .mockResolvedValueOnce(
        model({
          schemaRows: [{ name: "cost", values: ["ten"], type: "number", source: "Team" }],
          perFieldValidity: [
            {
              field: "cost",
              state: "invalid",
              message: "cost value 'ten' is not a number",
              code: "INVALID_TAG_VALUE",
            },
          ],
        }),
      )
      .mockResolvedValue(
        model({
          schemaRows: [{ name: "cost", values: ["42"], type: "number", source: "Team" }],
          perFieldValidity: [{ field: "cost", state: "valid", message: null, code: null }],
        }),
      );
    renderBand(false);
    const input = (await screen.findByLabelText("cost")) as HTMLInputElement;
    expect(input.value).toBe("ten");
    fireEvent.change(input, { target: { value: "42" } });
    fireEvent.blur(input);
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "setField", field: "cost", value: "42" },
      ]),
    );
  });

  it("renders a declared field as a row, not in the unknown-fields disclosure (AC-7)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          { name: "risk", values: ["low"], type: "choice", allowedValues: ["low"], source: "Team" },
        ],
        unknownFields: [["foo", "bar"]],
        perFieldValidity: [{ field: "risk", state: "valid", message: null, code: null }],
      }),
    );
    await renderBandLoaded();
    expect(screen.getByRole("button", { name: "risk" })).toBeInTheDocument();
    // The disclosure counts only the genuinely-unknown key — never the declared row.
    expect(screen.getByText(/unknown fields \(1 preserved\)/)).toBeInTheDocument();
  });

  it("clears a declared field via clearField when blanked", async () => {
    applyMetadataEdit.mockResolvedValue(undefined);
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [{ name: "note", values: ["temp"], type: "text", source: "Team" }],
        perFieldValidity: [{ field: "note", state: "valid", message: null, code: null }],
      }),
    );
    renderBand(false);
    const input = (await screen.findByLabelText("note")) as HTMLInputElement;
    expect(input.value).toBe("temp");
    fireEvent.change(input, { target: { value: "" } });
    fireEvent.blur(input);
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [{ kind: "clearField", field: "note" }]),
    );
  });

  it("flags an invalid declared date field with the reason (AC-4)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [{ name: "due", values: ["someday"], type: "date", source: "Team" }],
        perFieldValidity: [
          {
            field: "due",
            state: "invalid",
            message: "due value 'someday' is not a date (use YYYY-MM-DD)",
            code: "INVALID_TAG_VALUE",
          },
        ],
      }),
    );
    renderBand(false);
    const input = (await screen.findByLabelText("due")) as HTMLInputElement;
    expect(input).toHaveAttribute("aria-invalid", "true");
    // The date control can't display the non-ISO value, but the flag names why.
    expect(screen.getByText("use YYYY-MM-DD")).toBeInTheDocument();
  });

  it("normalizes a non-canonical declared type (padded / underscore yes_no) to the yes/no select", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        // A hand-authored field-schema.yml may carry casing/whitespace/underscore.
        schemaRows: [{ name: "blocking", values: ["maybe"], type: " Yes_No ", source: "Team" }],
        perFieldValidity: [
          {
            field: "blocking",
            state: "invalid",
            message: "blocking value 'maybe' is not yes or no",
            code: "INVALID_TAG_VALUE",
          },
        ],
      }),
    );
    renderBand(false);
    // A select renders as a button (not a text input); the flag reads the yes-no copy.
    const control = await screen.findByRole("button", { name: "blocking" });
    expect(control).toHaveTextContent("use yes or no");
    expect(screen.queryByLabelText("blocking")).not.toBeInstanceOf(HTMLInputElement);
  });

  it("normalizes an uppercase declared type (CHOICE) to the choice select with the typed flag", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "risk",
            values: ["cosmic"],
            type: "CHOICE",
            allowedValues: ["low", "high"],
            source: "Team",
          },
        ],
        perFieldValidity: [
          {
            field: "risk",
            state: "invalid",
            message: "risk value 'cosmic' is not an allowed value",
            code: "INVALID_TAG_VALUE",
          },
        ],
      }),
    );
    renderBand(false);
    const control = await screen.findByRole("button", { name: "risk" });
    // Uppercase type still resolves the choice control + the choice-specific flag,
    // not the free-text fallback / generic "invalid".
    expect(control).toHaveTextContent("not allowed");
    expect(screen.queryByLabelText("risk")).not.toBeInstanceOf(HTMLInputElement);
  });

  // ===== SW-AU-77J: add a declared-but-absent field from the drawer ==========

  it("SW-AU-77J: the + Add field menu offers declared-but-absent customs, not present ones (AC-1)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        // `risk` is present (a declared row); `effort` is declared but absent.
        schemaRows: [{ name: "risk", values: ["hi"], type: "text", source: "Team" }],
        declarableFields: [{ name: "effort", type: "text", required: false }],
      }),
    );
    await renderBandLoaded();
    fireEvent.click(screen.getByText("Add field"));
    // The absent custom is offered as a menu entry…
    expect(await screen.findByRole("button", { name: "effort" })).toBeInTheDocument();
    // …a built-in is still offered…
    expect(screen.getByRole("button", { name: "story_id" })).toBeInTheDocument();
    // …and the PRESENT declared field is not offered (it's already a row, an input).
    expect(screen.queryByRole("button", { name: "risk" })).not.toBeInTheDocument();
  });

  it("SW-AU-77J: picking a declared-absent field renders an empty typed row (AC-1)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ declarableFields: [{ name: "effort", type: "text", required: false }] }),
    );
    await renderBandLoaded();
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByRole("button", { name: "effort" }));
    // The empty typed row appears; the menu entry is gone (already added).
    const input = (await screen.findByLabelText("effort")) as HTMLInputElement;
    expect(input.value).toBe("");
    expect(input).not.toBeDisabled();
  });

  it("SW-AU-77J: filling an added declared-absent row commits via setField (AC-2)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ declarableFields: [{ name: "effort", type: "text", required: false }] }),
    );
    await renderBandLoaded();
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByRole("button", { name: "effort" }));
    const input = await screen.findByLabelText("effort");
    fireEvent.change(input, { target: { value: "3d" } });
    fireEvent.blur(input);
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "setField", field: "effort", value: "3d" },
      ]),
    );
  });

  it("SW-AU-77J: a required absent field shows the needs-a-value hint, no invalid flag (AC-4)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ declarableFields: [{ name: "approval", type: "text", required: true }] }),
    );
    await renderBandLoaded();
    fireEvent.click(screen.getByText("Add field"));
    const entry = await screen.findByRole("button", { name: /approval/ });
    expect(entry).toHaveTextContent("needs a value");
    // It's only an offer — no invalid badge anywhere in the drawer.
    expect(screen.queryByText("not allowed")).not.toBeInTheDocument();
  });

  it("SW-AU-77J: discarding an added-but-empty row writes nothing (AC-5)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ declarableFields: [{ name: "effort", type: "text", required: false }] }),
    );
    await renderBandLoaded();
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByRole("button", { name: "effort" }));
    expect(await screen.findByLabelText("effort")).toBeInTheDocument();
    // Discard the row the user added by mistake.
    fireEvent.click(screen.getByRole("button", { name: "Remove effort" }));
    await waitFor(() => expect(screen.queryByLabelText("effort")).not.toBeInTheDocument());
    // Nothing was ever written for the untouched row.
    expect(applyMetadataEdit).not.toHaveBeenCalled();
  });

  it("SW-AU-77J: adding a declared-absent row writes nothing until it's filled (AC-5)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ declarableFields: [{ name: "effort", type: "text", required: false }] }),
    );
    await renderBandLoaded();
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByRole("button", { name: "effort" }));
    // The row is present but untouched — closing/leaving it must persist no empty key.
    expect(await screen.findByLabelText("effort")).toBeInTheDocument();
    expect(applyMetadataEdit).not.toHaveBeenCalled();
  });

  it("SW-AU-77J: an added row suppresses the empty-doc note (AC-1)", async () => {
    // A plain doc (no known fields, no declared/unknown) shows the "No metadata yet" note.
    getMetadataModel.mockResolvedValue(
      model({
        fields: {},
        declarableFields: [{ name: "effort", type: "text", required: false }],
      }),
    );
    await renderBandLoaded();
    expect(screen.getByText(/No metadata yet/)).toBeInTheDocument();
    // Once the user adds a declared-absent row, the "plain doc" note must yield to it.
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByRole("button", { name: "effort" }));
    expect(await screen.findByLabelText("effort")).toBeInTheDocument();
    expect(screen.queryByText(/No metadata yet/)).not.toBeInTheDocument();
  });

  it("SW-AU-77J: a filled-then-cleared added field does not spontaneously re-show an empty row", async () => {
    applyMetadataEdit.mockResolvedValue(undefined);
    getMetadataModel
      // 1) initial: effort is declared-but-absent
      .mockResolvedValueOnce(
        model({ declarableFields: [{ name: "effort", type: "text", required: false }] }),
      )
      // 2) after fill: effort graduated to a present declared row
      .mockResolvedValueOnce(
        model({ schemaRows: [{ name: "effort", values: ["3d"], type: "text", source: "Team" }] }),
      )
      // 3) after clear: effort is absent again (offered in the menu, not auto-shown)
      .mockResolvedValue(
        model({ declarableFields: [{ name: "effort", type: "text", required: false }] }),
      );
    await renderBandLoaded();
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByRole("button", { name: "effort" }));
    const added = await screen.findByLabelText("effort");
    fireEvent.change(added, { target: { value: "3d" } });
    fireEvent.blur(added);
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "setField", field: "effort", value: "3d" },
      ]),
    );
    // Now a present row holding "3d"; clear it back to empty.
    const present = (await screen.findByLabelText("effort")) as HTMLInputElement;
    await waitFor(() => expect(present.value).toBe("3d"));
    fireEvent.change(present, { target: { value: "" } });
    fireEvent.blur(present);
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "clearField", field: "effort" },
      ]),
    );
    // The now-absent field must NOT re-appear as an empty added row.
    await waitFor(() => expect(screen.queryByLabelText("effort")).not.toBeInTheDocument());
  });

  it("SW-AU-77J: a doc turning read-only mid-session clears an unfilled added row (AC-5)", async () => {
    getMetadataModel.mockResolvedValue(
      model({ declarableFields: [{ name: "effort", type: "text", required: false }] }),
    );
    const utils = await renderBandLoaded(false);
    fireEvent.click(screen.getByText("Add field"));
    fireEvent.click(await screen.findByRole("button", { name: "effort" }));
    expect(await screen.findByLabelText("effort")).toBeInTheDocument();
    // The doc flips to read-only (e.g. now viewing another layer) — the unfilled,
    // now-undiscardable row must not be stranded.
    utils.rerender(
      <MetadataBand
        filePath={FILE}
        repoPath={REPO}
        orgRoot="/org"
        orgSlug="acme"
        team="default"
        sourceRepoOptions={[]}
        knownTeams={["default"]}
        readOnly
        onClose={vi.fn()}
        regionId="region"
      />,
    );
    await waitFor(() => expect(screen.queryByLabelText("effort")).not.toBeInTheDocument());
  });
});

describe("MetadataBand — org-declared tag rows (SW-AU-77I)", () => {
  it("renders a present org select tag as a typed row (select/ordered/status → select control)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "context_level",
            values: ["cosmic"],
            type: "ordered",
            allowedValues: ["low", "high"],
            color: "--cs-tag-ordered",
            icon: "Gauge",
            source: "Org",
          },
        ],
        perFieldValidity: [
          {
            field: "context_level",
            state: "invalid",
            message: "context_level value 'cosmic' is not in the allowed set",
            code: "INVALID_TAG_VALUE",
          },
        ],
      }),
    );
    renderBand(false);
    // `ordered` renders the closed-set select (a button), not a text input.
    const control = await screen.findByRole("button", { name: "context_level" });
    expect(control).toHaveTextContent("not allowed");
    expect(control).toHaveAttribute("aria-invalid", "true");
    expect(control).toHaveAttribute("aria-describedby");
    expect(screen.queryByLabelText("context_level")).not.toBeInstanceOf(HTMLInputElement);
  });

  it("renders a multi_select org tag as a chip editor, committing setList", async () => {
    applyMetadataEdit.mockResolvedValue(undefined);
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "owners",
            values: ["alice", "bob"],
            type: "multi_select",
            allowedValues: ["alice", "bob", "carol"],
            source: "Org",
          },
        ],
        perFieldValidity: [{ field: "owners", state: "valid", message: null, code: null }],
      }),
    );
    renderBand(false);
    const group = await screen.findByRole("group", { name: "owners" });
    expect(group).toBeInTheDocument();
    expect(screen.getByText("alice")).toBeInTheDocument();
    expect(screen.getByText("bob")).toBeInTheDocument();
    // Removing a chip commits the remaining list via setList (never a widened scalar).
    fireEvent.click(screen.getByRole("button", { name: "Remove alice" }));
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "setList", field: "owners", items: ["bob"] },
      ]),
    );
  });

  it("flags a multi_select org tag at row level with the reason (AC-5)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "owners",
            values: ["alice", "mallory"],
            type: "multi_select",
            allowedValues: ["alice", "bob"],
            source: "Org",
          },
        ],
        perFieldValidity: [
          {
            field: "owners",
            state: "invalid",
            message: "owners value 'mallory' is not in the allowed set",
            code: "INVALID_TAG_VALUE",
          },
        ],
      }),
    );
    renderBand(false);
    const group = await screen.findByRole("group", { name: "owners" });
    expect(group).toHaveAttribute("aria-invalid", "true");
    expect(group).toHaveAttribute("aria-describedby");
    expect(screen.getByText("not allowed")).toBeInTheDocument();
    expect(
      screen.getByText("owners value 'mallory' is not in the allowed set"),
    ).toBeInTheDocument();
  });

  it("renders team rows before org rows (AC-9)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          { name: "risk", values: ["low"], type: "choice", allowedValues: ["low"], source: "Team" },
          {
            name: "priority",
            values: ["p0"],
            type: "select",
            allowedValues: ["p0", "p1"],
            source: "Org",
          },
        ],
        perFieldValidity: [],
      }),
    );
    await renderBandLoaded();
    const risk = screen.getByRole("button", { name: "risk" });
    const priority = screen.getByRole("button", { name: "priority" });
    // The team row precedes the org row in document order.
    expect(risk.compareDocumentPosition(priority) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("renders a leading glyph on an org row label but not on a team row (AC-8)", async () => {
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "context_level",
            values: ["low"],
            type: "ordered",
            allowedValues: ["low", "high"],
            color: "--cs-tag-ordered",
            source: "Org",
          },
          {
            name: "risk",
            values: ["low"],
            type: "choice",
            allowedValues: ["low", "high"],
            source: "Team",
          },
        ],
        perFieldValidity: [],
      }),
    );
    await renderBandLoaded();
    // The org row's label carries a leading (aria-hidden) glyph svg; the team row's
    // label is plain text with no glyph.
    expect(screen.getByText("context_level").querySelector("svg")).toBeTruthy();
    expect(screen.getByText("risk").querySelector("svg")).toBeNull();
  });

  it("clears a multi_select org tag via clearField when the last chip is removed", async () => {
    applyMetadataEdit.mockResolvedValue(undefined);
    getMetadataModel.mockResolvedValue(
      model({
        schemaRows: [
          {
            name: "owners",
            values: ["alice"],
            type: "multi_select",
            allowedValues: ["alice", "bob"],
            source: "Org",
          },
        ],
        perFieldValidity: [{ field: "owners", state: "valid", message: null, code: null }],
      }),
    );
    renderBand(false);
    fireEvent.click(await screen.findByRole("button", { name: "Remove alice" }));
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "clearField", field: "owners" },
      ]),
    );
  });
});

describe("validate_story findings strip", () => {
  it("shows a visible finding for a story file missing `type: story`", async () => {
    getMetadataModel.mockResolvedValue(
      model({ fields: { story_id: "SW-1", source_repo: "acme/api", status: "todo" } }),
    );
    validateStoryFile.mockResolvedValue({
      detectedKind: "pipeline_story",
      format: "markdown",
      valid: false,
      findings: [
        {
          code: "INVALID_TYPE",
          field: "type",
          severity: "blocking",
          message: "no `type:` field — a pipeline story needs one",
          hint: "Add `type: story`.",
        },
      ],
    });
    renderBand();
    const strip = await screen.findByTestId("metadata-band-story-findings");
    expect(strip).toHaveTextContent("a pipeline story needs one");
    expect(strip.querySelector('[data-code="INVALID_TYPE"]')).not.toBeNull();
    expect(validateStoryFile).toHaveBeenCalledWith(FILE);
  });

  it("shows nothing for a clean story or a plain doc", async () => {
    getMetadataModel.mockResolvedValue(model());
    await renderBandLoaded();
    await waitFor(() => expect(validateStoryFile).toHaveBeenCalled());
    expect(screen.queryByTestId("metadata-band-story-findings")).toBeNull();
  });
});

it("saves a live taxonomy suggestion's id through metadata editing", async () => {
  const { useStatusTaxonomyStore } = await import("@/shared/stores/status-taxonomy-store");
  useStatusTaxonomyStore.getState().setStates([
    {
      id: "design",
      label: "Needs design",
      role: "not_started",
      order: 0,
      color: "red",
      icon: "Circle",
    },
    {
      id: "in-progress",
      label: "In progress",
      role: "active",
      order: 1,
      color: "blue",
      icon: "Circle",
    },
  ]);
  try {
    getMetadataModel.mockResolvedValue(model());
    renderBand();
    const input = await screen.findByLabelText("status");
    fireEvent.focus(input);
    expect(screen.getByRole("option", { name: "In progress" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      screen.getByRole("option", { name: "In progress" }).querySelector("[data-menu-check] svg"),
    ).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "design" } });
    fireEvent.click(screen.getByRole("option", { name: /Needs design/ }));
    await waitFor(() =>
      expect(applyMetadataEdit).toHaveBeenCalledWith(FILE, [
        { kind: "setField", field: "status", value: "design" },
      ]),
    );
  } finally {
    useStatusTaxonomyStore.getState().resetForTest();
  }
});
