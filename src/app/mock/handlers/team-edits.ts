// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { TeamFileEdit, TeamFileEditsStatus, TeamFileEditsWorkspace } from "@/tauri/commands";

/**
 * SW-XS-27: the "N unsaved" team edits world, opt-in with `?edits=demo` (on top of
 * any other world, e.g. `?nav=demo&edits=demo`). A workspace on This Mac with a
 * changed story and a new note, a machine workspace with a file changed here and in
 * the team, and an offline machine's last count. Publish and Discard take files off
 * the list; View reads both sides from the texts below.
 */
export function isTeamEditsDemo(search: string): boolean {
  return ["demo", "missing"].includes(new URLSearchParams(search).get("edits") ?? "");
}

const TEAM = "/mock/team-content";
const LOCAL = "/mock/worktrees/SW-BSI-9";

const TEXTS: Record<string, string> = {
  [`${TEAM}/_bmad-output/stories/SW-BSI-9.md`]:
    "# SW-BSI-9: Live update race\n\n## Purpose\nThe pill flickers when two updates race.\n\n## Acceptance criteria\n1. One update wins.\n",
  [`${LOCAL}/_bmad-output/stories/SW-BSI-9.md`]:
    "# SW-BSI-9: Live update race\n\n## Purpose\nThe pill flickers when two updates race; the later one must win.\n\n## Acceptance criteria\n1. The later update wins.\n2. No flicker under load.\n",
  [`${LOCAL}/_bmad-output/notes/race-repro.md`]: "Repro: open two tabs, edit both within 50 ms.\n",
  [`${TEAM}/CLAUDE.md`]: "# Team rules\n\n- Ship small PRs.\n- Review before merge.\n",
};

function file(
  path: string,
  kind: TeamFileEdit["kind"],
  extra: Partial<TeamFileEdit> = {},
): TeamFileEdit {
  return {
    path,
    kind,
    destination: { kind: "team", name: "default" },
    teamPath: `${TEAM}/${path}`,
    ...extra,
  };
}

function seed(now: number): TeamFileEditsWorkspace[] {
  return [
    {
      target: { kind: "local", codeRepoPath: LOCAL },
      name: "SW-BSI-9",
      state: "checked",
      checkedAtMs: now,
      files: [
        file("_bmad-output/stories/SW-BSI-9.md", "changed", { changedAtMs: now - 4 * 60_000 }),
        file("_bmad-output/notes/race-repro.md", "new", { changedAtMs: now - 2 * 60_000 }),
        file(".claude/skills/my-helper/SKILL.md", "new", { destination: { kind: "private" } }),
      ],
    },
    {
      target: {
        kind: "remote",
        machineId: "pi-box",
        workspaceHandle: `remote-workspace-${"a".repeat(64)}`,
      },
      name: "SW-RM-4",
      cwd: "/home/dev/work/SW-RM-4",
      state: "checked",
      machineOnline: true,
      checkedAtMs: now - 12 * 60_000,
      files: [
        file("CLAUDE.md", "conflict", {
          reason:
            "the team file changed since it was copied here (base 0123456789ab, team ba9876543210)",
        }),
      ],
    },
    {
      target: {
        kind: "remote",
        machineId: "laptop-2",
        workspaceHandle: `remote-workspace-${"b".repeat(64)}`,
      },
      name: "SW-AGR-7",
      cwd: "/home/dev/work/SW-AGR-7",
      state: "offline",
      machineOnline: false,
      checkedAtMs: now - 2 * 60 * 60_000,
      reason: "the machine is not connected; this is its last count",
      files: [file("_bmad-output/stories/SW-AGR-7.md", "changed")],
    },
  ];
}

export function teamEditsHandlers(search: string, base: HandlerRegistry): HandlerRegistry {
  if (!isTeamEditsDemo(search)) {
    // Every other world: nothing unsaved (the counter stays hidden).
    return { team_file_edits_status: () => ({ total: 0, cleanWorkspaces: 0, workspaces: [] }) };
  }
  let workspaces = seed(Date.now());
  if (new URLSearchParams(search).get("edits") === "missing") {
    workspaces.unshift({
      target: {
        kind: "remote",
        machineId: "test-1",
        workspaceHandle: `remote-workspace-${"c".repeat(64)}`,
      },
      name: "SW-AN-55-P5",
      state: "missing",
      machineOnline: true,
      machineName: "codesense-test-1",
      files: [],
      counts: { changed: 3, new: 0, removed: 0, conflicts: 0, cannotPublish: 0 },
    });
  }
  const status = (): TeamFileEditsStatus => {
    for (const ws of workspaces) {
      if (ws.state === "missing") continue;
      const files = ws.files ?? [];
      ws.counts = {
        changed: files.filter((f) => f.kind === "changed").length,
        new: files.filter((f) => f.kind === "new").length,
        removed: files.filter((f) => f.kind === "removed").length,
        conflicts: files.filter((f) => f.kind === "conflict").length,
        cannotPublish: files.filter((f) => f.kind === "cannotPublish").length,
      };
    }
    const listed = workspaces.filter((w) => (w.files ?? []).length > 0 || w.state !== "checked");
    return {
      total: listed.reduce((n, w) => n + (w.files?.length ?? 0), 0),
      cleanWorkspaces: workspaces.length - listed.length,
      workspaces: listed,
    };
  };
  const drop = (target: Record<string, unknown>, paths: string[]) => {
    workspaces = workspaces.map((w) => {
      const same =
        w.target.kind === "local"
          ? target.kind === "local" && target.codeRepoPath === w.target.codeRepoPath
          : target.kind === "remote" && target.workspaceHandle === w.target.workspaceHandle;
      return same ? { ...w, files: (w.files ?? []).filter((f) => !paths.includes(f.path)) } : w;
    });
  };
  return {
    team_file_edits_status: (args) => {
      const count = args.count as { kind: string; workspaceHandle?: string } | undefined;
      if (count?.kind === "remote") {
        workspaces = workspaces.map((w) =>
          w.target.kind === "remote" && w.target.workspaceHandle === count.workspaceHandle
            ? { ...w, checkedAtMs: Date.now() }
            : w,
        );
      }
      return status();
    },
    forget_missing_workspace: (args) => {
      const target = args.target as { machineId: string; workspaceHandle: string };
      const found = workspaces.find(
        (ws) =>
          ws.target.kind === "remote" &&
          ws.target.machineId === target.machineId &&
          ws.target.workspaceHandle === target.workspaceHandle,
      );
      if (!found || found.state !== "missing")
        return {
          status: "refused",
          state: found?.state ?? "unknown",
          reason: "the workspace is not missing",
        };
      workspaces = workspaces.filter((ws) => ws !== found);
      return { status: "removed" };
    },
    publish_context_edits: (args) => {
      drop(args.target as Record<string, unknown>, args.paths as string[]);
      return { status: "published", failedCount: 0, conflicts: [] };
    },
    discard_team_file_edits: (args) => {
      drop(args.target as Record<string, unknown>, args.paths as string[]);
      return { status: "ready", failedCount: 0 };
    },
    read_file: (args) => {
      const path = String(args.path ?? "");
      if (path in TEXTS) return TEXTS[path];
      if (path.startsWith(TEAM) || path.startsWith(LOCAL)) {
        throw new Error("No such file or directory (os error 2)");
      }
      return base.read_file?.(args);
    },
    source_remote_file_read: (args) => {
      if (args.path === "CLAUDE.md") {
        const text =
          "# Team rules\n\n- Ship small PRs.\n- Review before merge, always.\n- Keep stories current.\n";
        return { base64: btoa(text), observation: {}, truncated: false };
      }
      return base.source_remote_file_read?.(args);
    },
  };
}
