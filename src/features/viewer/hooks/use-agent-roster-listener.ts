// SPDX-License-Identifier: MIT
import { useAgentListFactsMirror } from "./use-agent-list-facts";
import { useEffect } from "react";
import { watchAgentListInputs } from "../lib/agent-list-inputs";
import { watchAgentList } from "../lib/agent-list-projection";

/** Keep the shared owner projections warm for native agent surfaces. */
export function useAgentRosterListener(): void {
  useEffect(watchAgentListInputs, []);
  useEffect(watchAgentList, []);
  useAgentListFactsMirror();
}
