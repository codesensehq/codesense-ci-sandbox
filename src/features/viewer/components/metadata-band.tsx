// SPDX-License-Identifier: MIT
import { fileChangeAffects } from "@/shared/lib/file-change-affects";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { StoryFileFindings } from "@/features/story-authoring/components/story-file-findings";
import {
  Check,
  AlertTriangle,
  Undo2,
  RefreshCw,
  Lock,
  Plus,
  ChevronRight,
  ChevronDown as ChevronDownIcon,
  X,
  Loader2,
} from "lucide-react";

import {
  getMetadataModel,
  applyMetadataEdit,
  canDeclareTeamField,
  type MetadataModel,
  type MetadataEdit,
  type SourceRepoOption,
  type FieldValidity,
  type DeclareGate,
  type SchemaFieldRow,
} from "@/tauri/commands";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { MENU_STYLE } from "@/shared/components/floating-menu";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { logger } from "@/shared/lib/logger";
import { getErrorMessage } from "@/shared/lib/error-message";
import { buildTeamKey } from "@/shared/lib/team-key";
import { toast } from "sonner";

import {
  TYPE_OPTIONS,
  EPIC_ONLY_FIELDS,
  RETIRED_FIELDS,
  STORY_ONLY_FIELDS,
  KNOWN_FIELD_ORDER,
  isListField,
  isFieldPresent,
  fieldScalarValue,
  fieldListValue,
  type KnownField,
  type ScalarField,
  type ListField,
} from "../lib/metadata-schema";
import {
  MetadataOptionSelect,
  MetadataStatusCombobox,
  MetadataSourceRepoPicker,
  MetadataTextField,
} from "./metadata-field-controls";
import { MetadataListChipEditor } from "./metadata-list-chip-editor";
import { TagValueControl } from "./tag-value-control";
import { rowDisplay } from "@/shared/lib/tag-schema/types";
import { resolveLucideIcon } from "@/shared/lib/icons/lucide-icon-map";
import { AddCustomFieldForm } from "./add-custom-field-form";
import { useDeclareTeamField, type DeclareFieldArgs } from "@/shared/hooks/use-declare-team-field";
import { AssigneePicker } from "@/shared/components/assignee-picker";

interface MetadataBandProps {
  /** Absolute path of the open doc. */
  filePath: string;
  /** Active repo path — used to bump the file-refresh store after a write. */
  repoPath: string;
  /** Org context for validity resolution (configured repos / teams). */
  orgRoot?: string;
  orgSlug?: string;
  team?: string;
  /** Configured repos for the active team (attached sorted first). */
  sourceRepoOptions: SourceRepoOption[];
  /** Known team names for the `team` select. */
  knownTeams: readonly string[];
  /** AC-13: open read-only (controls disabled, header note). */
  readOnly: boolean;
  /** Close the band (Esc / chip toggle) — returns focus to the chip. */
  onClose: () => void;
  /** id of the region for aria-controls wiring on the chip. */
  regionId: string;
}

type SaveState = "idle" | "saving" | "saved";

function findValidity(model: MetadataModel | null, field: string): FieldValidity | undefined {
  return model?.perFieldValidity.find((v) => v.field === field);
}

/** SW-AU-77G: the yes/no option set a declared `yes-no` field picks from. */
/** SW-AU-77I: the tinted, aria-hidden leading glyph for an ORG row's label (AC-8);
 *  team rows get none. Resolved from the row itself — no schema lookup. */
function orgRowIcon(row: SchemaFieldRow): ReactNode {
  if (row.source !== "Org") return undefined;
  const display = rowDisplay(row);
  const Icon = resolveLucideIcon(display.icon);
  return <Icon size={11} className="flex-shrink-0" style={{ color: display.color }} aria-hidden />;
}

/**
 * SLW-MD-1 (Task 5.2, 5.3, 6, 7, 8): the full-width collapsible metadata band.
 * Reads the format-aware model, renders per-field controls, auto-saves each
 * commit, mirrors external changes, and falls back to a raw editor on a
 * malformed block.
 */
export function MetadataBand({
  filePath,
  repoPath,
  orgRoot,
  orgSlug,
  team,
  sourceRepoOptions,
  knownTeams,
  readOnly,
  onClose,
  regionId,
}: MetadataBandProps) {
  const [model, setModel] = useState<MetadataModel | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [undoStack, setUndoStack] = useState<MetadataEdit[] | null>(null);
  const [showAddMenu, setShowAddMenu] = useState(false);
  const [showUnknown, setShowUnknown] = useState(false);
  const [externalChange, setExternalChange] = useState(false);
  const [rawDraft, setRawDraft] = useState("");

  // Tracks whether a field is mid-edit (focused/uncommitted) so an external
  // change shows the passive Reload banner instead of clobbering (AC-14).
  const editingRef = useRef(false);
  // Tracks an in-flight save so a refresh bump from our OWN write doesn't
  // spuriously flag an external change.
  const savingOwnWriteRef = useRef(false);
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const m = await getMetadataModel(filePath, orgRoot, orgSlug, team);
      setModel(m);
      setExternalChange(false);
      if (!m.blockParseOk) {
        // Seed the raw editor from the verbatim on-disk block (AC-12). The
        // backend populates `rawBlock` even when the block failed to parse — so
        // the user sees and repairs the ACTUAL broken text.
        setRawDraft(m.rawBlock ?? "");
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logger.warn("metadata-band", `getMetadataModel failed for ${filePath}: ${msg}`);
      setLoadError(msg);
      setModel(null);
    } finally {
      setLoading(false);
    }
  }, [filePath, orgRoot, orgSlug, team]);

  // (Re)load on file change (AC-15: band stays open, re-reads for the new doc).
  useEffect(() => {
    void load();
  }, [load]);

  // AC-14: watch the refresh store. When the open file changes on disk and we
  // have no in-flight uncommitted edit, silently re-read; otherwise show the
  // passive Reload banner (never clobber).
  //
  // SR-9: gate on `contentRefreshKeys` (the viewer content-version), NOT the
  // `fileRefreshKeys` scan counter. The metadata band is a content reader of the
  // OPEN file (`getMetadataModel` re-reads it from disk); a `counterOnly`
  // ContentLayerEvent materialize storm bumps `fileRefreshKeys` ~1×/sec without
  // changing this file, and the `changedSet === null` broad-sentinel guard below
  // does NOT catch it (counterOnly leaves `lastChangedContextFile` at its resting
  // null). Keying on `contentRefreshKeys` — which a counterOnly bump never advances
  // — makes the storm a no-op for the band too, matching the viewer body.
  const changedSet = useFileRefreshStore((s) => s.lastChangedContextFile[repoPath] ?? null);
  const refreshKey = useFileRefreshStore((s) => s.contentRefreshKeys[repoPath] ?? 0);
  const lastRefreshKeyRef = useRef(refreshKey);
  useEffect(() => {
    if (refreshKey === lastRefreshKeyRef.current) return;
    lastRefreshKeyRef.current = refreshKey;
    // Our own write bumped the store — already re-read below; skip.
    if (savingOwnWriteRef.current) {
      savingOwnWriteRef.current = false;
      return;
    }
    // Only react if THIS file changed (or the broad-refresh sentinel).
    const affectsThisFile = fileChangeAffects(changedSet, filePath);
    if (!affectsThisFile) return;
    if (editingRef.current) {
      setExternalChange(true);
    } else {
      void load();
    }
  }, [refreshKey, changedSet, filePath, load]);

  const runEdits = useCallback(
    async (edits: MetadataEdit[], inverse: MetadataEdit[] | null) => {
      setSaveState("saving");
      savingOwnWriteRef.current = true;
      try {
        await applyMetadataEdit(filePath, edits);
        // Re-read to get the fresh model + validity, then bump so nav + editor
        // re-render (Task 7.1).
        const m = await getMetadataModel(filePath, orgRoot, orgSlug, team);
        setModel(m);
        setUndoStack(inverse);
        setSaveState("saved");
        useFileRefreshStore.getState().bumpFileRefresh(repoPath, new Set([filePath]));
        if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
        saveTimerRef.current = setTimeout(() => setSaveState("idle"), 1800);
      } catch (e) {
        savingOwnWriteRef.current = false;
        const msg = e instanceof Error ? e.message : String(e);
        logger.warn("metadata-band", `applyMetadataEdit failed for ${filePath}: ${msg}`);
        setSaveState("idle");
      }
    },
    [filePath, repoPath, orgRoot, orgSlug, team],
  );

  // SW-AU-77E: declare a typed team field from the drawer, then set its value on THIS
  // story in the same action (AC-6). The declare hook validates + persists the schema
  // (RMW-safe); the value write reuses the atomic
  // `runEdits` path (re-reads the model + bumps). Off the webview thread throughout (AC-9).
  const canDeclare = Boolean(orgRoot && team && !readOnly);
  const declareField = useDeclareTeamField(orgRoot ?? "", team ?? "");
  const [declareGate, setDeclareGate] = useState<DeclareGate | null>(null);
  useEffect(() => {
    if (readOnly || !orgRoot || !team) {
      setDeclareGate(null);
      return;
    }
    let alive = true;
    void canDeclareTeamField(orgRoot, team)
      .then((g) => alive && setDeclareGate(g))
      .catch(() => alive && setDeclareGate(null));
    return () => {
      alive = false;
    };
  }, [orgRoot, team, readOnly]);

  const declareAndSet = useCallback(
    async (args: DeclareFieldArgs) => {
      await declareField(args);
      if (args.value?.trim()) {
        // Write the value DIRECTLY, not via `runEdits` (which swallows write errors): a
        // set-failure after a successful declare must surface to the form, never a silent
        // half-success (AC-6). `savingOwnWriteRef` keeps our own bump from tripping the
        // external-change banner, matching `runEdits`.
        savingOwnWriteRef.current = true;
        try {
          await applyMetadataEdit(filePath, [
            { kind: "setField", field: args.name, value: args.value },
          ]);
        } catch (e) {
          // Reset the self-write guard on failure (mirrors `runEdits`) so a stuck flag can't
          // swallow the NEXT external-change notification; rethrow so the form shows the error.
          savingOwnWriteRef.current = false;
          throw e;
        }
        useFileRefreshStore.getState().bumpFileRefresh(repoPath, new Set([filePath]));
      }
      await load();
    },
    [declareField, filePath, repoPath, load],
  );

  // --- per-field commit helpers ------------------------------------------
  const commitScalar = useCallback(
    (field: ScalarField, value: string | null) => {
      if (!model || readOnly) return;
      const prev = fieldScalarValue(model.fields, field);
      if (value === prev) return;
      const edit: MetadataEdit =
        value == null ? { kind: "clearField", field } : { kind: "setField", field, value };
      const inverse: MetadataEdit =
        prev == null ? { kind: "clearField", field } : { kind: "setField", field, value: prev };
      void runEdits([edit], [inverse]);
    },
    [model, readOnly, runEdits],
  );

  const commitList = useCallback(
    (field: ListField, items: string[]) => {
      if (!model || readOnly) return;
      const prev = fieldListValue(model.fields, field);
      const edit: MetadataEdit =
        items.length === 0 ? { kind: "clearField", field } : { kind: "setList", field, items };
      const inverse: MetadataEdit =
        prev.length === 0 ? { kind: "clearField", field } : { kind: "setList", field, items: prev };
      void runEdits([edit], [inverse]);
    },
    [model, readOnly, runEdits],
  );

  // SW-AU-77G: commit a team-declared custom field's value. Unlike `commitScalar`,
  // the previous value comes from the declared row (custom keys aren't on the typed
  // `model.fields`); the write reuses the same arbitrary-`field` `setField` path.
  const commitDeclared = useCallback(
    (name: string, prev: string, value: string | null) => {
      if (!model || readOnly) return;
      const next = value?.trim() ? value.trim() : null;
      if ((next ?? "") === prev) return;
      const edit: MetadataEdit =
        next == null
          ? { kind: "clearField", field: name }
          : { kind: "setField", field: name, value: next };
      const inverse: MetadataEdit = { kind: "setField", field: name, value: prev };
      void runEdits([edit], [inverse]);
    },
    [model, readOnly, runEdits],
  );

  // SW-AU-77I: commit a declared `multi_select` tag's per-item list. Like
  // `commitDeclared` (arbitrary key, not on the typed `model.fields`), the previous
  // list is passed in from the row's own `values`; the write reuses the same
  // `setList` path `commitList` uses for the known list fields.
  const commitDeclaredList = useCallback(
    (name: string, prev: string[], items: string[]) => {
      if (!model || readOnly) return;
      const edit: MetadataEdit =
        items.length === 0
          ? { kind: "clearField", field: name }
          : { kind: "setList", field: name, items };
      const inverse: MetadataEdit =
        prev.length === 0
          ? { kind: "clearField", field: name }
          : { kind: "setList", field: name, items: prev };
      void runEdits([edit], [inverse]);
    },
    [model, readOnly, runEdits],
  );

  const removeUnknown = useCallback(
    (key: string) => {
      if (readOnly) return;
      void runEdits([{ kind: "clearField", field: key }], null);
    },
    [readOnly, runEdits],
  );

  const doUndo = useCallback(() => {
    if (!undoStack) return;
    const inverse = undoStack;
    setUndoStack(null);
    void runEdits(inverse, null);
  }, [undoStack, runEdits]);

  // --- raw-block recovery (AC-12) ----------------------------------------
  const [applyingRaw, setApplyingRaw] = useState(false);
  const applyRaw = useCallback(async () => {
    setApplyingRaw(true);
    setSaveState("saving");
    savingOwnWriteRef.current = true;
    try {
      await applyMetadataEdit(filePath, [{ kind: "replaceRawBlock", text: rawDraft }]);
      const m = await getMetadataModel(filePath, orgRoot, orgSlug, team);
      setModel(m);
      useFileRefreshStore.getState().bumpFileRefresh(repoPath, new Set([filePath]));
      setSaveState("saved");
      setTimeout(() => setSaveState("idle"), 1800);
      // If it re-parsed we snap back to the structured view automatically
      // (model.blockParseOk is now true).
    } catch (e) {
      savingOwnWriteRef.current = false;
      const msg = e instanceof Error ? e.message : String(e);
      logger.warn("metadata-band", `replaceRawBlock failed for ${filePath}: ${msg}`);
      setSaveState("idle");
    } finally {
      setApplyingRaw(false);
    }
  }, [filePath, rawDraft, repoPath, orgRoot, orgSlug, team]);

  // --- Esc closes + returns focus (AC / Task 5.2) ------------------------
  const bandRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape" && !editingRef.current) {
        // Only close when a nested control isn't itself consuming Escape.
        onClose();
      }
    }
    const el = bandRef.current;
    el?.addEventListener("keydown", onKey);
    return () => el?.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    };
  }, []);

  const format = model?.format ?? "markdown";
  const formatLabel = format === "html" ? "HTML meta tags" : "markdown frontmatter";
  const invalidCount = model?.perFieldValidity.filter((v) => v.state === "invalid").length ?? 0;
  const blockOk = model?.blockParseOk ?? true;

  // SW-WB-1 (AC-9): the branch-mismatch signal is a PURE frontmatter compare —
  // intended (`branch_name`) vs observed (`dev_branch`, auto-stamped every
  // establish). No git call on render. It only fires once a worktree exists
  // (dev_branch present) and the intent diverged.
  //
  // SW-OR-1b: the intended/observed split is retired in favor of a single
  // canonical `branch`. Once that field is present (new stories always; a legacy
  // story after any open/reconcile stamps it), there is no two-field mismatch to
  // show — so suppress the chip. This both matches the unified model AND lets the
  // legacy reconcile clear the chip: reconcile stamps `branch` via establish.
  const canonicalBranch = model ? fieldScalarValue(model.fields, "branch") : null;
  const intendedBranch = model ? fieldScalarValue(model.fields, "branch_name") : null;
  const observedBranch = model ? fieldScalarValue(model.fields, "dev_branch") : null;
  const branchMismatch =
    !canonicalBranch && !!intendedBranch && !!observedBranch && intendedBranch !== observedBranch;
  const [reconciling, setReconciling] = useState(false);

  // SW-WB-1 (AC-5): one-click reconcile the worktree onto the intended branch.
  // Store access is a lazy dynamic import so the band's module graph stays light
  // (and its unit tests don't pull the whole viewer store). Fully async /
  // non-blocking — the reconcile itself runs behind the store's open overlay.
  const runReconcile = useCallback(
    async (opts?: { forceRecreate?: boolean }) => {
      if (!orgSlug || reconciling) return;
      const teamKey = buildTeamKey(orgSlug, team ?? null);
      setReconciling(true);
      try {
        const { useViewerStore } = await import("@/shared/stores/viewer-store");
        const res = await useViewerStore
          .getState()
          .reconcileWorktreeBranch(teamKey, filePath, opts);
        if (res.mode === "migrate") {
          toast.success(`Worktree migrated onto ${intendedBranch}`);
        } else if (res.mode === "recreate") {
          toast.success(`Worktree rebuilt on ${intendedBranch}`);
        }
        // Re-read so the re-stamped canonical `branch` (SW-OR-1b) clears the
        // mismatch chip.
        editingRef.current = false;
        void load();
      } catch (e) {
        // AC-7: a dirty/diverged tree makes the in-place migrate refuse rather
        // than clobber. Surface the typed reason AND offer an explicit
        // recreate-that-discards (never auto-discards). Suppress the offer on a
        // forced attempt to avoid a loop.
        if (opts?.forceRecreate) {
          toast.error(getErrorMessage(e));
        } else {
          toast.error(getErrorMessage(e), {
            action: {
              label: "Recreate (discards local changes)",
              onClick: () => void runReconcile({ forceRecreate: true }),
            },
          });
        }
      } finally {
        setReconciling(false);
      }
    },
    [orgSlug, team, filePath, intendedBranch, reconciling, load],
  );

  const handleReconcile = useCallback(() => void runReconcile(), [runReconcile]);

  return (
    <div
      id={regionId}
      ref={bandRef}
      role="region"
      aria-label="Document metadata editor"
      className="border-b"
      style={{ backgroundColor: "var(--band-bg, var(--muted))", borderColor: "var(--border)" }}
    >
      {/* header */}
      <div
        className="flex items-center gap-2.5 border-b px-3.5 py-2 text-[11px]"
        style={{
          backgroundColor: "var(--band-header, var(--muted))",
          borderColor: "var(--border)",
        }}
      >
        <span
          className="font-bold tracking-wider uppercase"
          style={{ color: "var(--muted-foreground)" }}
        >
          Metadata
        </span>
        <span style={{ color: "var(--muted-foreground)" }}>
          ({formatLabel}
          {!blockOk ? " — unparseable" : ""})
        </span>
        <span className="flex-1" />

        {saveState === "saving" && (
          <span
            className="inline-flex items-center gap-1"
            style={{ color: "var(--muted-foreground)" }}
          >
            <Loader2 size={12} className="animate-spin" />
            saving…
          </span>
        )}
        {saveState === "saved" && (
          <span className="inline-flex items-center gap-1" style={{ color: "var(--ok, #16a34a)" }}>
            <Check size={12} />
            saved
          </span>
        )}
        {blockOk && invalidCount > 0 && (
          <span
            className="inline-flex items-center gap-1 font-semibold"
            style={{ color: "var(--amber, #d97706)" }}
          >
            <AlertTriangle size={12} />
            {invalidCount} issue{invalidCount === 1 ? "" : "s"}
          </span>
        )}
        {!readOnly && undoStack && (
          <button
            type="button"
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-black/5"
            style={{ color: "var(--muted-foreground)" }}
            onClick={doUndo}
          >
            <Undo2 size={12} />
            Undo
          </button>
        )}
        {readOnly && (
          <span
            className="inline-flex items-center gap-1.5 italic"
            style={{ color: "var(--muted-foreground)" }}
          >
            <Lock size={12} />
            Viewing metadata from another layer — read-only
          </span>
        )}
      </div>

      {model && <StoryFileFindings filePath={filePath} revision={model} />}

      {/* external-change banner (AC-14) */}
      {externalChange && (
        <div
          className="flex items-center gap-2.5 px-3.5 py-2 text-[12px]"
          style={{
            backgroundColor: "var(--amber-bg, var(--muted))",
            color: "var(--amber, #d97706)",
            borderBottom: "1px solid var(--amber-border, var(--border))",
          }}
        >
          <RefreshCw size={14} />
          <span>File changed on disk (external agent / reverse-sync). Your edit is preserved.</span>
          <span className="flex-1" />
          <button
            type="button"
            className="rounded border px-2.5 py-0.5 text-[11.5px] font-semibold"
            style={{ borderColor: "currentColor" }}
            onClick={() => {
              editingRef.current = false;
              void load();
            }}
          >
            Reload
          </button>
        </div>
      )}

      {/* SW-WB-1 (AC-9): branch-mismatch chip + one-click reconcile. */}
      {branchMismatch && (
        <div
          className="flex items-center gap-2.5 px-3.5 py-2 text-[12px]"
          style={{
            backgroundColor: "var(--amber-bg, var(--muted))",
            color: "var(--amber, #d97706)",
            borderBottom: "1px solid var(--amber-border, var(--border))",
          }}
          role="status"
          aria-label="branch mismatch"
        >
          <AlertTriangle size={14} />
          <span>
            Worktree on <code className="font-semibold">{observedBranch}</code> · story says{" "}
            <code className="font-semibold">{intendedBranch}</code>
          </span>
          <span className="flex-1" />
          {!readOnly && orgSlug && (
            <button
              type="button"
              className="inline-flex items-center gap-1 rounded border px-2.5 py-0.5 text-[11.5px] font-semibold disabled:opacity-60"
              style={{ borderColor: "currentColor" }}
              onClick={handleReconcile}
              disabled={reconciling}
            >
              {reconciling && <Loader2 size={12} className="animate-spin" />}
              Reconcile
            </button>
          )}
        </div>
      )}

      {loading && !model ? (
        <div className="px-3.5 py-3 text-[12px]" style={{ color: "var(--muted-foreground)" }}>
          Loading metadata…
        </div>
      ) : loadError ? (
        <div className="px-3.5 py-3 text-[12px]" style={{ color: "var(--danger, #dc2626)" }}>
          Couldn&rsquo;t read metadata: {loadError}
        </div>
      ) : model && !blockOk ? (
        <RawBlockEditor
          value={rawDraft}
          onChange={setRawDraft}
          onApply={applyRaw}
          onCancel={() => void load()}
          applying={applyingRaw}
          disabled={readOnly}
        />
      ) : model ? (
        // Keyed by the open file so the form's session state (added built-in rows,
        // added declared-absent rows, the new-field form) resets on a doc switch —
        // otherwise a picked-but-unfilled row would leak onto the next document.
        <StructuredForm
          key={filePath}
          model={model}
          readOnly={readOnly}
          sourceRepoOptions={sourceRepoOptions}
          knownTeams={knownTeams}
          showAddMenu={showAddMenu}
          setShowAddMenu={setShowAddMenu}
          showUnknown={showUnknown}
          setShowUnknown={setShowUnknown}
          onEditingChange={(editing) => {
            editingRef.current = editing;
          }}
          commitScalar={commitScalar}
          commitList={commitList}
          commitDeclared={commitDeclared}
          commitDeclaredList={commitDeclaredList}
          removeUnknown={removeUnknown}
          findValidity={(f) => findValidity(model, f)}
          canDeclare={canDeclare}
          declareDisabledReason={
            declareGate?.allowed === false ? (declareGate.reason ?? null) : null
          }
          onDeclareField={declareAndSet}
        />
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Structured form (the normal, parse-ok view).
// ---------------------------------------------------------------------------
interface StructuredFormProps {
  model: MetadataModel;
  readOnly: boolean;
  sourceRepoOptions: SourceRepoOption[];
  knownTeams: readonly string[];
  showAddMenu: boolean;
  setShowAddMenu: (v: boolean) => void;
  showUnknown: boolean;
  setShowUnknown: (v: boolean) => void;
  onEditingChange: (editing: boolean) => void;
  commitScalar: (field: ScalarField, value: string | null) => void;
  commitList: (field: ListField, items: string[]) => void;
  /** SW-AU-77G: commit a team-declared custom field's value (arbitrary key). */
  commitDeclared: (name: string, prev: string, value: string | null) => void;
  /** SW-AU-77I: commit a declared `multi_select` tag's per-item list (arbitrary key). */
  commitDeclaredList: (name: string, prev: string[], items: string[]) => void;
  removeUnknown: (key: string) => void;
  findValidity: (field: string) => FieldValidity | undefined;
  /** SW-AU-77E: whether this context can declare a typed team field (offers "New field…"). */
  canDeclare: boolean;
  /** AC-5: a truthful reason the declare action is disabled (e.g. read-only tree), or null. */
  declareDisabledReason: string | null;
  /** Declare a typed field and set its value on the current story (AC-6). */
  onDeclareField: (args: DeclareFieldArgs) => Promise<void>;
}

/** One metadata row.
 *
 * SW-VE-16 Clean minimal / IDE-native (Mockup #2 + R3 fix 2 carry-forward from #3):
 * - 140 px key column, left-aligned.
 * - Key renders **mono** (12 px) so field names read as code; values stay sans so prose
 *   reads as prose. The split is lossless — mono keys lock enum-type apart from freeform
 *   values at a glance (R3 reviewer item 2).
 * - 32 px row min-height with a per-row hover tint, so a long schema is scannable without
 *   feeling cramped (mockup #2 recipe).
 *
 * `action` renders a trailing affordance (e.g. a discard button) beside the control. The
 * single owner of the band's row layout — built-in, declared, and added rows all render
 * through it.
 */
function MetadataRow({
  label,
  labelIcon,
  action,
  children,
}: {
  label: string;
  /** SW-AU-77I: optional tinted leading glyph on the label (an org tag's icon). */
  labelIcon?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="grid min-h-[32px] grid-cols-[140px_1fr] items-center gap-x-3 gap-y-1 rounded px-1.5 py-1 transition-colors hover:bg-accent/40">
      <span
        className="flex items-center gap-1 font-mono text-[12px]"
        style={{ color: "var(--muted-foreground)" }}
      >
        {labelIcon}
        {label}
      </span>
      {action ? (
        <div className="flex min-w-0 items-center gap-1.5">
          <div className="min-w-0 flex-1">{children}</div>
          {action}
        </div>
      ) : (
        <div className="min-w-0">{children}</div>
      )}
    </div>
  );
}

/** One "+ Add field" menu entry: a field name plus an optional right-aligned hint
 *  (e.g. "promote to story", "needs a value"). Picking it adds the field and closes the
 *  menu. Shared by the built-in and declared-but-absent lists so they can't drift. */
function AddFieldMenuItem({
  label,
  hint,
  onPick,
}: {
  label: string;
  hint?: string;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      className="flex w-full items-center gap-2 rounded px-2 py-1 text-[12px] hover:bg-black/5"
      onClick={onPick}
    >
      {label}
      {hint && (
        <span className="ml-auto text-[10px]" style={{ color: "var(--muted-foreground)" }}>
          {hint}
        </span>
      )}
    </button>
  );
}

/** A small icon button that removes a row. Shared by the unknown-field remove and the
 *  added declared-row discard so both carry the same hover + keyboard-focus affordance. */
function RemoveButton({
  label,
  onClick,
  className,
}: {
  label: string;
  onClick: () => void;
  className?: string;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      className={`rounded p-0.5 hover:bg-black/5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring${
        className ? ` ${className}` : ""
      }`}
      style={{ color: "var(--muted-foreground)" }}
      onClick={onClick}
    >
      <X size={12} />
    </button>
  );
}

function StructuredForm({
  model,
  readOnly,
  sourceRepoOptions,
  knownTeams,
  showAddMenu,
  setShowAddMenu,
  showUnknown,
  setShowUnknown,
  onEditingChange,
  commitScalar,
  commitList,
  commitDeclared,
  commitDeclaredList,
  removeUnknown,
  findValidity,
  canDeclare,
  declareDisabledReason,
  onDeclareField,
}: StructuredFormProps) {
  const { fields, isEpic, unknownFields, schemaRows, declarableFields } = model;
  // SW-AU-77E: the "+ Add field" menu toggles to the typed new-field form.
  const [showNewField, setShowNewField] = useState(false);

  // A field is rendered when it's present, OR the user just added it.
  const [added, setAdded] = useState<Set<KnownField>>(new Set());
  const isRendered = useCallback(
    (field: KnownField) => isFieldPresent(fields, field) || added.has(field),
    [fields, added],
  );

  // SW-AU-77J: declared customs the user picked from "+ Add field" this session but
  // hasn't filled yet. A separate set (not `added`, which is the closed `KnownField`
  // enum) since these are arbitrary custom-field names.
  const [addedDeclared, setAddedDeclared] = useState<Set<string>>(new Set());
  const discardAddedDeclared = useCallback(
    (name: string) =>
      setAddedDeclared((s) => {
        const next = new Set(s);
        next.delete(name);
        return next;
      }),
    [],
  );

  // Once an added field is filled, the re-read lifts it into `schemaRows` as a
  // present row — drop it from the session set so its intent is consumed. Otherwise
  // clearing that field later (it leaves `schemaRows`) would spontaneously re-show
  // an empty added row the user didn't re-request; instead it returns to "+ Add field".
  // Returns the same Set reference when nothing graduated, so this never re-loops.
  useEffect(() => {
    setAddedDeclared((s) => {
      let next: Set<string> | null = null;
      for (const row of schemaRows) {
        if (s.has(row.name)) {
          next ??= new Set(s);
          next.delete(row.name);
        }
      }
      return next ?? s;
    });
  }, [schemaRows]);

  // A doc that turns read-only mid-session can no longer have its added-but-unfilled
  // rows filled or discarded, so drop the session-added rows rather than strand an
  // empty, unremovable control. Same-ref return when already empty → no re-loop.
  useEffect(() => {
    if (!readOnly) return;
    setAdded((s) => (s.size ? new Set<KnownField>() : s));
    setAddedDeclared((s) => (s.size ? new Set<string>() : s));
  }, [readOnly]);

  // Whether the "+ Add field" menu OFFERS a field for THIS doc kind: epic-only
  // fields only on epics, story-only fields (assigned_to) only on non-epics
  // (SW-ID-5), retired fields never (SW-XS-5). Gates OFFERING only — a field already present still renders (see
  // `renderedKnown`), so a stray value is never stranded/unremovable.
  const offerableOnDoc = useCallback(
    (f: KnownField) =>
      !RETIRED_FIELDS.has(f) &&
      (!EPIC_ONLY_FIELDS.has(f) || isEpic) &&
      (!STORY_ONLY_FIELDS.has(f) || !isEpic),
    [isEpic],
  );

  // Fields available in "+ Add field": known, not present, not already added,
  // and offerable for this doc kind. `type` is included (adding it is how a
  // plain doc is promoted to a story — AC-16).
  const addable = KNOWN_FIELD_ORDER.filter((f) => !isRendered(f) && offerableOnDoc(f));

  // SW-AU-77J: declared-but-absent team customs offered by "+ Add field" (backend
  // projection, AC-3), minus any already picked this session.
  const declarableAddable = declarableFields.filter((d) => !addedDeclared.has(d.name));

  // Picked-but-unfilled declared customs, rendered as EMPTY typed rows by reusing
  // `renderSchemaRow` (a synthetic empty-valued row). Dedup against present rows so a
  // just-filled field — which the next read lifts into `schemaRows` — renders once.
  // Declarable fields are team customs (org-tag names are excluded backend-side), so
  // these are always `source: "Team"` (no glyph).
  const addedDeclaredRows: SchemaFieldRow[] = Array.from(addedDeclared)
    .filter((name) => !schemaRows.some((row) => row.name === name))
    .map((name) => {
      const def = declarableFields.find((d) => d.name === name);
      return {
        name,
        values: [],
        type: def?.type,
        allowedValues: def?.allowedValues,
        source: "Team" as const,
      };
    });

  const renderControl = (field: KnownField) => {
    const validity = findValidity(field);
    const invalid = validity?.state === "invalid";
    if (field === "type") {
      return (
        <MetadataOptionSelect
          ariaLabel="type"
          value={fieldScalarValue(fields, "type")}
          options={TYPE_OPTIONS}
          disabled={readOnly}
          onChange={(v) => commitScalar("type", v)}
        />
      );
    }
    if (field === "status") {
      return (
        <MetadataStatusCombobox
          ariaLabel="status"
          value={fieldScalarValue(fields, "status")}
          disabled={readOnly}
          onChange={(v) => commitScalar("status", v)}
        />
      );
    }
    if (field === "team") {
      return (
        <MetadataOptionSelect
          ariaLabel="team"
          value={fieldScalarValue(fields, "team")}
          options={knownTeams}
          disabled={readOnly}
          onChange={(v) => commitScalar("team", v)}
        />
      );
    }
    if (field === "assigned_to") {
      // SW-ID-5: the ONE shared picker; commitScalar supplies optimistic revert.
      return (
        <AssigneePicker
          ariaLabel="assigned_to"
          value={fieldScalarValue(fields, "assigned_to")}
          disabled={readOnly}
          onChange={(v) => commitScalar("assigned_to", v)}
        />
      );
    }
    if (field === "source_repo") {
      const flag = validity?.code === "MALFORMED_SOURCE_REPO" ? "malformed" : "not recognized";
      return (
        <MetadataSourceRepoPicker
          ariaLabel="source_repo"
          value={fieldScalarValue(fields, "source_repo")}
          options={sourceRepoOptions}
          invalid={invalid}
          invalidFlag={flag}
          disabled={readOnly}
          onChange={(v) => commitScalar("source_repo", v)}
        />
      );
    }
    if (field === "story_id" || field === "branch" || field === "branch_name") {
      // SW-WB-1: branch_name is the editable INTENDED branch (drives the
      // worktree); renders as a plain text field like story_id/branch.
      return (
        <MetadataTextField
          ariaLabel={field}
          value={fieldScalarValue(fields, field)}
          disabled={readOnly}
          onChange={(v) => commitScalar(field, v)}
        />
      );
    }
    if (field === "dev_branch") {
      // SW-WB-1: dev_branch is the OBSERVED branch the worktree is actually on
      // (auto-written every establish). It is a READ-ONLY mirror — the editable
      // intent lives in `branch_name` above — so a deliberate edit is never
      // silently reverted by the establish write-back. Always disabled,
      // independent of `readOnly`.
      return (
        <MetadataTextField
          ariaLabel="dev_branch"
          value={fieldScalarValue(fields, "dev_branch")}
          disabled
          onChange={() => {}}
        />
      );
    }
    if (isListField(field)) {
      return (
        <MetadataListChipEditor
          ariaLabel={field}
          items={fieldListValue(fields, field)}
          disabled={readOnly}
          onChange={(items) => commitList(field, items)}
        />
      );
    }
    return null;
  };

  // SW-AU-77G/77I: a present schema-declared field (team custom OR org tag) as its
  // own typed control, choosing by declared type and reusing the SAME inline invalid
  // + flag treatment the built-ins use (driven by the shared backend validity). The
  // FE reads the row's own `type`/`values`/`allowedValues` — no schema lookup.
  const renderSchemaRow = (row: SchemaFieldRow) => {
    const validity = findValidity(row.name);
    // One shared control + dispatch (SW-AU-78 T3): the drawer and the composer rule
    // builder select the same control, in the same declared order, with the same
    // invalid flag. The scalar path commits `row.values[0]`; multi_select commits the
    // per-item list.
    return (
      <TagValueControl
        type={row.type}
        values={row.values}
        allowedValues={row.allowedValues}
        ariaLabel={row.name}
        disabled={readOnly}
        invalid={validity?.state === "invalid"}
        invalidReason={validity?.message ?? undefined}
        onChangeScalar={(v) => commitDeclared(row.name, row.values[0] ?? "", v)}
        onChangeList={(items) => commitDeclaredList(row.name, row.values, items)}
      />
    );
  };

  // Render every PRESENT known field (a present value is always visible + editable
  // so it can be corrected/cleared) — `appliesToDoc` gates only what the "+ Add
  // field" menu OFFERS. This keeps a stray story-only field (e.g. a hand-authored
  // `assigned_to` on an epic) removable rather than stranding it invisibly.
  const renderedKnown = KNOWN_FIELD_ORDER.filter(isRendered);
  const isEmpty =
    renderedKnown.length === 0 &&
    schemaRows.length === 0 &&
    addedDeclaredRows.length === 0 &&
    unknownFields.length === 0;

  return (
    <div
      className="px-3.5 pt-3 pb-3.5"
      onFocusCapture={() => onEditingChange(true)}
      onBlurCapture={(e) => {
        // Only clear editing when focus leaves the whole form.
        if (!e.currentTarget.contains(e.relatedTarget as Node)) onEditingChange(false);
      }}
    >
      {isEmpty && (
        <div
          className="mb-2 text-center text-[12px] italic"
          style={{ color: "var(--muted-foreground)" }}
        >
          No metadata yet — this is a plain doc (valid, no badge).
        </div>
      )}

      <div className="flex flex-col gap-2">
        {renderedKnown.map((field) => (
          <MetadataRow key={field} label={field}>
            {renderControl(field)}
          </MetadataRow>
        ))}

        {/* SW-AU-77G/77I: present schema-declared fields — team customs then org
            tags — as first-class typed rows with the same live inline invalid flag
            the built-ins use. An org tag carries a small tinted (aria-hidden) leading
            glyph on its label (AC-8); team rows stay plain, unchanged from 77G. */}
        {schemaRows.map((row) => (
          <MetadataRow key={row.name} label={row.name} labelIcon={orgRowIcon(row)}>
            {renderSchemaRow(row)}
          </MetadataRow>
        ))}

        {/* SW-AU-77J: declared-but-absent fields the user added this session — the
            SAME typed row (empty), plus a discard control that writes nothing (AC-5). */}
        {addedDeclaredRows.map((row) => (
          <MetadataRow
            key={row.name}
            label={row.name}
            action={
              !readOnly && (
                <RemoveButton
                  label={`Remove ${row.name}`}
                  onClick={() => discardAddedDeclared(row.name)}
                />
              )
            }
          >
            {renderSchemaRow(row)}
          </MetadataRow>
        ))}
      </div>

      {/* footer: unknown-fields disclosure + Add field */}
      <div
        className="mt-3 flex items-center gap-3 border-t border-dashed pt-2.5"
        style={{ borderColor: "var(--border)" }}
      >
        <button
          type="button"
          className="inline-flex items-center gap-1.5 text-[11.5px]"
          style={{
            color: "var(--muted-foreground)",
            opacity: unknownFields.length === 0 ? 0.5 : 1,
          }}
          disabled={unknownFields.length === 0}
          aria-expanded={showUnknown}
          onClick={() => setShowUnknown(!showUnknown)}
        >
          {showUnknown ? <ChevronDownIcon size={12} /> : <ChevronRight size={12} />}
          unknown fields ({unknownFields.length} preserved)
        </button>

        {!readOnly && (
          <div className="ml-auto">
            <Popover open={showAddMenu} onOpenChange={setShowAddMenu}>
              <PopoverTrigger asChild>
                <button
                  type="button"
                  className="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11.5px]"
                  style={{ borderColor: "var(--border)", color: "var(--foreground)" }}
                >
                  <Plus size={12} />
                  Add field
                </button>
              </PopoverTrigger>
              <PopoverContent
                align="end"
                side="bottom"
                className="max-h-60 min-w-[200px] gap-0 overflow-y-auto rounded-md border p-1 shadow-lg ring-0"
                style={MENU_STYLE}
              >
                {showNewField ? (
                  <AddCustomFieldForm
                    disabledReason={declareDisabledReason}
                    onDeclare={onDeclareField}
                    onClose={() => {
                      setShowNewField(false);
                      setShowAddMenu(false);
                    }}
                  />
                ) : (
                  <>
                    {addable.length === 0 && declarableAddable.length === 0 ? (
                      <div
                        className="px-2 py-1 text-[11px]"
                        style={{ color: "var(--muted-foreground)" }}
                      >
                        All fields present.
                      </div>
                    ) : (
                      <>
                        {addable.map((f) => (
                          <AddFieldMenuItem
                            key={f}
                            label={f}
                            hint={f === "type" ? "promote to story" : undefined}
                            onPick={() => {
                              setAdded((s) => new Set(s).add(f));
                              setShowAddMenu(false);
                            }}
                          />
                        ))}
                        {/* SW-AU-77J: declared-but-absent team customs — pick one to
                            drop in its empty typed row. A required one hints (never blocks). */}
                        {declarableAddable.map((d) => (
                          <AddFieldMenuItem
                            key={`declared-${d.name}`}
                            label={d.name}
                            hint={d.required ? "needs a value" : undefined}
                            onPick={() => {
                              setAddedDeclared((s) => new Set(s).add(d.name));
                              setShowAddMenu(false);
                            }}
                          />
                        ))}
                      </>
                    )}
                    {/* SW-AU-77E: declare a NEW typed team field, right from the drawer. */}
                    {canDeclare && (
                      <button
                        type="button"
                        data-testid="metadata-add-custom-field"
                        className="mt-0.5 flex w-full items-center gap-2 rounded border-t px-2 py-1 text-[12px] hover:bg-black/5"
                        style={{ borderColor: "var(--border)" }}
                        onClick={() => setShowNewField(true)}
                      >
                        <Plus size={12} />
                        New field…
                      </button>
                    )}
                  </>
                )}
              </PopoverContent>
            </Popover>
          </div>
        )}
      </div>

      {/* unknown fields expanded (round-tripped verbatim). */}
      {showUnknown && unknownFields.length > 0 && (
        <div
          className="mt-2 rounded-md border p-2"
          style={{ backgroundColor: "var(--muted)", borderColor: "var(--border)" }}
        >
          {unknownFields.map(([key, val]) => (
            <div key={key} className="flex items-center gap-2 py-0.5 text-[11.5px]">
              <span className="font-mono font-semibold" style={{ color: "var(--foreground)" }}>
                {key}:
              </span>
              <span className="font-mono" style={{ color: "var(--muted-foreground)" }}>
                {val}
              </span>
              {!readOnly && (
                <RemoveButton
                  label={`Remove ${key}`}
                  onClick={() => removeUnknown(key)}
                  className="ml-auto"
                />
              )}
            </div>
          ))}
          <div className="mt-1 text-[10.5px]" style={{ color: "var(--muted-foreground)" }}>
            Round-tripped verbatim on every save — never dropped.
          </div>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Raw-block editor (AC-12): explicit Apply/Cancel; the ONLY explicit-apply case.
// ---------------------------------------------------------------------------
interface RawBlockEditorProps {
  value: string;
  onChange: (v: string) => void;
  onApply: () => void;
  onCancel: () => void;
  applying: boolean;
  disabled: boolean;
}

function RawBlockEditor({
  value,
  onChange,
  onApply,
  onCancel,
  applying,
  disabled,
}: RawBlockEditorProps) {
  return (
    <div>
      <div
        className="flex items-center gap-2.5 px-3.5 py-2 text-[12px]"
        style={{
          backgroundColor: "var(--danger-bg, var(--muted))",
          color: "var(--danger, #dc2626)",
          borderBottom: "1px solid var(--danger-border, var(--border))",
        }}
      >
        <AlertTriangle size={14} />
        <span>
          This metadata block can&rsquo;t be parsed. Fix it below, then Apply. Structured editing
          returns once it re-parses.
        </span>
      </div>
      <textarea
        aria-label="Raw metadata block"
        className="m-3.5 block w-[calc(100%-1.75rem)] rounded-md border p-3 font-mono text-[12px] leading-relaxed outline-none focus-visible:ring-2 focus-visible:ring-[var(--ring)]"
        style={{
          backgroundColor: "var(--code-bg, var(--muted))",
          borderColor: "var(--danger-border, var(--border))",
          color: "var(--foreground)",
          minHeight: 140,
        }}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
      <div className="flex justify-end gap-2 px-3.5 pb-3.5">
        <button
          type="button"
          className="rounded-md border px-3.5 py-1 text-[12px]"
          style={{ borderColor: "var(--border)", color: "var(--foreground)" }}
          onClick={onCancel}
          disabled={applying}
        >
          Cancel
        </button>
        <button
          type="button"
          className="rounded-md border px-3.5 py-1 text-[12px] font-semibold text-white"
          style={{
            backgroundColor: "var(--ring)",
            borderColor: "var(--ring)",
            opacity: disabled || applying ? 0.5 : 1,
          }}
          onClick={onApply}
          disabled={disabled || applying}
        >
          {applying ? "Applying…" : "Apply"}
        </button>
      </div>
    </div>
  );
}
