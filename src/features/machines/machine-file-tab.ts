// SPDX-License-Identifier: MIT
import { createElement } from "react";

import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { RemoteFileViewer } from "./components/remote-file-viewer";
import { basename } from "@/shared/lib/basename";

import {
  MACHINE_FILE_PREFIX,
  machineFilePath,
  parseMachineFilePath,
  type MachineFileRef,
} from "./lib/machine-file-path";
import { machineQualifiedPath } from "./lib/machine-status";
import { findMachine } from "./stores/machines-store";

const labelOf = (machineId: string): string => findMachine(machineId)?.label ?? machineId;

/** `🖥 devbox-01 · server.rs` — a remote tab always names its machine; a local
 *  tab never carries a mark, so a mark always means remote. */
function machineFileTabTitle(path: string): string {
  const ref = parseMachineFilePath(path);
  return ref ? `🖥 ${labelOf(ref.machineId)} · ${basename(ref.absPath)}` : path;
}

function machineFileTabTooltip(path: string): string {
  const ref = parseMachineFilePath(path);
  return ref ? machineQualifiedPath({ label: labelOf(ref.machineId) }, ref.absPath) : path;
}

let registered = false;

/** Register remote doc tabs (`virtual://machine-file/…`) with the viewer. Idempotent. */
export function registerMachineFileTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(
    MACHINE_FILE_PREFIX,
    machineFileTabTitle,
    ({ filePath }) => {
      const ref = parseMachineFilePath(filePath);
      return ref
        ? createElement(RemoteFileViewer, { key: filePath, file: ref })
        : createElement(
            "p",
            { className: "p-3 text-xs text-muted-foreground" },
            "Unknown remote file",
          );
    },
    machineFileTabTooltip,
  );
}

/** Open a file ON a machine as a read-only doc tab in `teamKey`'s viewer. */
export function openMachineFile(teamKey: string, file: MachineFileRef): void {
  registerMachineFileTab();
  useViewerStore.getState().openFile(teamKey, "", machineFilePath(file));
}
