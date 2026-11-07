// SPDX-License-Identifier: MIT
import { useEffect, type ReactNode } from "react";
import { useTheme } from "next-themes";

import { TooltipProvider } from "@/components/ui/tooltip";
import { OperationChipView, OperationDetails } from "@/features/shell/components/operation-chip";
import {
  useContextOperationsStore,
  type ContextOperation,
} from "@/features/shell/stores/context-operations-store";
import {
  SetupFailureActions,
  SetupFailureDetails,
  SetupLineView,
} from "@/features/viewer/components/setup-line";
import { WorkspacePill } from "@/features/viewer/components/workspace-pill";
import { KeptModifiedNote } from "@/features/viewer/components/kept-modified-note";
import {
  describeSetup,
  TERMINAL_SERVER_OUTDATED,
  type WorkspaceSetupState,
} from "@/features/viewer/lib/workspace-setup-state";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

/**
 * `dev:mock` only: every state of the SW-XS-10 progress UI on one page — the
 * workspace setup line (Step 1 phases, the wait for the agent, Step 2, Ready,
 * both failures), the tray pills, and the operation chip with its details —
 * for screenshots and the impeccable critique.
 * `?harness=progress[&theme=light|dark]`.
 */

const TEAM = "harness:progress";
const MB = 1024 * 1024;
const NAMES = { repo: "codesense", machine: "studio" };
const FAR = Date.now() + 365 * 24 * 3600 * 1000;

const LINE_STATES: Array<[string, WorkspaceSetupState]> = [
  ["Connecting", { kind: "step1", phase: "connecting", ...NAMES }],
  [
    "Fetching",
    { kind: "step1", phase: "fetching", done: 120, total: 480, unit: "objects", ...NAMES },
  ],
  ["Bundling", { kind: "step1", phase: "bundling", done: 480, unit: "objects", ...NAMES }],
  [
    "Copying",
    { kind: "step1", phase: "copying", done: 41 * MB, total: 112 * MB, unit: "bytes", ...NAMES },
  ],
  ["Verifying", { kind: "step1", phase: "verifying", ...NAMES }],
  ["Creating worktree", { kind: "step1", phase: "creatingWorktree", ...NAMES }],
  ["Running setup", { kind: "step1", phase: "runningSetup", detail: "pnpm install" }],
  ["Waiting for the agent", { kind: "awaitingAgent", since: FAR }],
  [
    "Preparing Barb",
    {
      kind: "step2",
      agent: "Barb",
      phase: "transfer",
      files: 312,
      bytes: 4.1 * MB,
      total: 6.8 * MB,
      startedAt: 0,
    },
  ],
  [
    "Two agents preparing (the busiest shows)",
    {
      kind: "step2",
      agent: "Mia",
      phase: "transfer",
      files: 840,
      bytes: 12.4 * MB,
      total: 30 * MB,
      startedAt: 0,
      alsoPreparing: 1,
    },
  ],
  ["Ready", { kind: "ready", at: FAR }],
];

const STEP1_FAILED: Extract<WorkspaceSetupState, { kind: "failed" }> = {
  kind: "failed",
  step: 1,
  phase: "copying",
  reason: "the connection to build-mac dropped",
};
const STEP2_FAILED: Extract<WorkspaceSetupState, { kind: "failed" }> = {
  kind: "failed",
  step: 2,
  phase: "prepare",
  agent: "Barb",
  reason: ".claude/skills/review/SKILL.md couldn't be written: permission denied",
  failed: [[".claude/skills/review/SKILL.md", "permission denied"]],
  note: "Prepared 311 of 312 files (4.1 MB) from team default in 2.3 s.",
};
const SERVER_FAILED: Extract<WorkspaceSetupState, { kind: "failed" }> = {
  kind: "failed",
  step: 2,
  phase: "prepare",
  agent: "Barb",
  reason: "Update the terminal server on studio: it is too old to sync agent context",
  code: TERMINAL_SERVER_OUTDATED,
};

const story = (id: string, over: Partial<Workspace> = {}): Workspace =>
  ({
    id,
    kind: "story",
    cwd: `/Users/dev/worktrees/${id}`,
    status: "ready",
    storyId: id,
    storyFilePath: `/Users/dev/context/${id}.md`,
    sourceRepoSlug: "alderco/codesense",
    ...over,
  }) as Workspace;

const remote = (id: string, over: Partial<Workspace> = {}): Workspace =>
  story(id, {
    cwd: "",
    location: {
      kind: "machine",
      machineId: "studio",
      requestId: "00000000-0000-4000-8000-000000000010",
      handle: `h-${id}`,
      cwd: `/home/dev/worktrees/${id}`,
    },
    ...over,
  });

const summary = {
  status: "ready" as const,
  wanted: 18,
  written: 18,
  bytesWritten: 221_000,
  unchanged: 294,
  keptModified: ["CLAUDE.md", ".claude/skills/dev/SKILL.md"],
  keptModifiedCount: 2,
  keptUnmanaged: [],
  keptUnmanagedCount: 0,
  skippedTracked: ["AGENTS.md"],
  skippedTrackedCount: 1,
  skippedOther: [],
  skippedOtherCount: 0,
  pinnedDrift: [],
  failed: [],
  failedCount: 0,
  leftovers: 0,
  elapsedMs: 600,
};

const op = (
  id: string,
  kind: "hydrate" | "publish",
  phase: ContextOperation["state"]["phase"],
  extra: Partial<ContextOperation["state"]> = {},
  settled = true,
): ContextOperation => ({
  id,
  workspace: `/Users/dev/worktrees/${id}`,
  caller: "Barb",
  startedAt: FAR - 600,
  ...(settled ? { settledAt: FAR } : {}),
  state: {
    workspace: `/Users/dev/worktrees/${id}`,
    agent: "Barb",
    operationId: id,
    operationKind: kind,
    phase,
    files: 18,
    bytes: 221_000,
    total: 18,
    settled,
    ...extra,
  },
});

const OPS: Array<[string, ContextOperation]> = [
  ["Hydrating", op("op-run", "hydrate", "transfer", { total: 440_000 }, false)],
  ["Publishing", op("op-pub", "publish", "transfer", { files: 3, total: 3 }, false)],
  ["Hydrated", op("op-done", "hydrate", "ready", { summary })],
  [
    "Published, with conflicts",
    op("op-partial", "publish", "ready", {
      files: 3,
      total: 5,
      note: "Published 3 change(s); 2 conflict(s) not written.",
    }),
  ],
  ["Failed", op("op-fail", "hydrate", "failed", { note: "studio went offline before the copy." })],
  ["Stopped", op("op-stop", "hydrate", "cancelled")],
];

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-3">
      <h2 className="text-sm font-semibold text-[var(--foreground)]">{title}</h2>
      {children}
    </section>
  );
}

function Tile({
  label,
  children,
  onApp = false,
}: {
  label: string;
  children: ReactNode;
  /** The failure placeholder sits on the app surface, not the terminal. */
  onApp?: boolean;
}) {
  return (
    <div className="space-y-1">
      <div className="text-[11px] text-[var(--muted-foreground)]">{label}</div>
      <div
        data-surface={onApp ? "canvas" : "terminal"}
        className={`flex h-24 items-center justify-center rounded border border-[var(--border)] px-6`}
        // The terminal is always dark (github-dark), whatever the app theme.
        style={{ backgroundColor: onApp ? "var(--background)" : "#0d1117" }}
      >
        {children}
      </div>
    </div>
  );
}

export function ProgressHarness() {
  const { setTheme } = useTheme();
  const themeParam = new URLSearchParams(window.location.search).get("theme");
  useEffect(() => {
    if (themeParam === "light" || themeParam === "dark") setTheme(themeParam);
  }, [themeParam, setTheme]);

  const pills = [
    story("SW-20", { status: "creating", setupState: LINE_STATES[3][1] }),
    remote("SW-21", {
      setupState: { kind: "awaitingAgent", since: FAR },
      agentRuns: {
        Barb: {
          kind: "running",
          agent: "Barb",
          phase: "transfer",
          files: 312,
          bytes: 4.1 * MB,
          total: 6.8 * MB,
          startedAt: 0,
        },
      },
    }),
    story("SW-22", {
      agentRuns: { Barb: { kind: "failed", agent: "Barb", reason: STEP2_FAILED.reason } },
    }),
    story("op-run"),
    story("op-done"),
    story("op-fail"),
  ];
  useEffect(() => {
    useViewerStore.setState({ workspaces: { [TEAM]: pills } });
    useContextOperationsStore.setState({ byId: Object.fromEntries(OPS.map(([, o]) => [o.id, o])) });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <TooltipProvider delayDuration={200}>
      <main className="min-h-screen space-y-8 bg-[var(--background)] p-8 text-xs text-[var(--foreground)]">
        <Section title="Workspace setup line — the creating cover and the tile corner">
          <div className="grid grid-cols-2 gap-4 lg:grid-cols-3">
            {LINE_STATES.map(([label, state]) => (
              <Tile key={label} label={label}>
                <SetupLineView view={describeSetup(state)!} variant="overlay" />
              </Tile>
            ))}
          </div>
        </Section>

        <Section title="Failures — Retry, Details, one-click server update">
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
            <Tile label="Step 1 failed (machine) — the workspace's error pane" onApp>
              <SetupLineView
                view={describeSetup(STEP1_FAILED)!}
                variant="pane"
                actions={
                  <SetupFailureActions ws={remote("SW-23")} teamKey={TEAM} state={STEP1_FAILED} />
                }
                details={<SetupFailureDetails state={STEP1_FAILED} />}
              />
            </Tile>
            <Tile label="Step 2 failed (no recorded launch → Launch Barb again)">
              <SetupLineView
                view={describeSetup(STEP2_FAILED)!}
                variant="corner"
                actions={
                  <SetupFailureActions ws={story("SW-22")} teamKey={TEAM} state={STEP2_FAILED} />
                }
                details={<SetupFailureDetails state={STEP2_FAILED} />}
                onDismiss={() => {}}
              />
            </Tile>
            <Tile label="Step 2 failed: terminal server too old">
              <SetupLineView
                view={describeSetup(SERVER_FAILED)!}
                variant="corner"
                actions={
                  <SetupFailureActions ws={remote("SW-24")} teamKey={TEAM} state={SERVER_FAILED} />
                }
              />
            </Tile>
          </div>
        </Section>

        <Section title="Kept because it was modified — Discard my edits">
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
            <Tile label="After a preparation (tile corner)">
              <KeptModifiedNote
                paths={["CLAUDE.md", ".claude/skills/dev/SKILL.md"]}
                count={2}
                target={{
                  kind: "local",
                  codeRepoPath: "/Users/dev/worktrees/SW-25",
                  sessionId: "s",
                }}
                onDismiss={() => {}}
              />
            </Tile>
          </div>
        </Section>

        <Section title="Tray pills — setup ring and operation chip">
          <div className="flex flex-wrap items-center gap-2 rounded border border-[var(--border)] bg-[var(--card)] p-2">
            {pills.map((p) => (
              <WorkspacePill
                key={p.id}
                workspace={p}
                isActive={p.id === "SW-21"}
                isStatusPopoverOpen={false}
                onStatusPopoverOpenChange={() => {}}
                onSelect={() => {}}
                onClose={() => {}}
              />
            ))}
          </div>
        </Section>

        <Section title="Operation chip — every state, and its details">
          <div className="flex flex-wrap items-center gap-3">
            {OPS.map(([label, o]) => (
              <div key={o.id} className="flex items-center gap-2" title={label}>
                <OperationChipView op={o} now={FAR - 1000} />
              </div>
            ))}
          </div>
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
            {[OPS[2][1], OPS[3][1], OPS[0][1]].map((o) => (
              <div
                key={o.id}
                className="w-80 rounded-lg bg-[var(--popover)] p-3 ring-1 ring-[var(--border)]"
              >
                <OperationDetails op={o} />
              </div>
            ))}
          </div>
        </Section>
      </main>
    </TooltipProvider>
  );
}
