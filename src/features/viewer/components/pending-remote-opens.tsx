// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Loader2, X } from "lucide-react";
import { toast } from "sonner";
import {
  allowAgentsAndRelaunch,
  openMachinesPanel,
  offerTerminalServerUpdate,
} from "@/features/machines/lib/machine-actions";
import { getErrorMessage } from "@/shared/lib/error-message";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import type { AgentRuntime } from "@/tauri/commands";
import {
  dismissRemoteOpen,
  recheckRemoteOpen,
  relaunchRefusedRemoteOpen,
  restartRefusedRemoteOpen,
  STALLED_OPEN_CODES,
  usePendingRemoteOpens,
  type PendingRemoteOpen,
} from "../lib/remote-tab-open";

/** The one line that installs each remotely-launchable agent CLI for the machine's account. */
export const AGENT_INSTALL_COMMAND = {
  claude: "curl -fsSL https://claude.ai/install.sh | bash",
  codex: "npm install -g @openai/codex",
} satisfies Partial<Record<AgentRuntime, string>>;

const LINK = "text-[#58a6ff] hover:underline";

/** The CLI a refused launch needs: a saved Agent's runtime is named only in
 *  the machine's answer ("OpenAI Codex isn't installed …"). */
function missingCli(open: PendingRemoteOpen): keyof typeof AGENT_INSTALL_COMMAND {
  return open.launch === "codex" || open.message?.includes("Codex") ? "codex" : "claude";
}

/** What is being opened, for the notice. */
function noun(open: PendingRemoteOpen, capital = false): string {
  const word = open.agentKey
    ? "Agent"
    : open.launch === "shell"
      ? "shell"
      : agentCliLabel(open.launch);
  return capital ? word.charAt(0).toUpperCase() + word.slice(1) : word;
}

/** An open with no answer within the settle time: offers recovery instead of
 *  spinning. Attach asks again by request id, so nothing is created twice. */
function stalled(open: PendingRemoteOpen): boolean {
  return open.status === "unknown" && STALLED_OPEN_CODES.includes(open.code ?? "");
}

/** What a refused open says: the machine's reason, in the person's words for
 *  the refusals they can act on. */
function refusalText(open: PendingRemoteOpen): string {
  if (open.code === "agentCliMissing")
    return `${missingCli(open) === "codex" ? "Codex" : "Claude"} isn't installed on ${open.label}`;
  if (open.code === "agentsNotAllowed") return `Agents aren't allowed on ${open.label} yet`;
  return `Couldn't open on ${open.label}: ${open.message}`;
}

/** A missing CLI: the command that installs it, to run in a shell there. */
function InstallCommand({ open }: { open: PendingRemoteOpen }) {
  const command = AGENT_INSTALL_COMMAND[missingCli(open)];
  return (
    <span className="flex min-w-0 items-center gap-2">
      <code
        data-testid="pending-remote-open-install"
        className="min-w-0 truncate rounded bg-[#0d1117] px-1.5 py-0.5 font-mono text-[11px] text-[#e6edf3]"
        title={command}
      >
        {command}
      </code>
      <button
        type="button"
        className={LINK}
        onClick={() => {
          void navigator.clipboard
            ?.writeText(command)
            .then(() => toast("Copied the install command"))
            .catch(() => {});
        }}
      >
        Copy
      </button>
      <button
        type="button"
        className={LINK}
        onClick={() => relaunchRefusedRemoteOpen(open.requestId)}
      >
        Try again
      </button>
    </span>
  );
}

/** Agents off for this machine: allow them (asking first), then launch again. */
function AllowAgents({ open, teamKey }: { open: PendingRemoteOpen; teamKey: string }) {
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  if (!asking)
    return (
      <button
        type="button"
        data-testid="pending-remote-open-allow-agents"
        className={LINK}
        onClick={() => setAsking(true)}
      >
        Allow agents on {open.label}
      </button>
    );
  return (
    <span className="flex items-center gap-2">
      <span className="text-[#8b949e]">Use CodeSense with your permissions there?</span>
      <button
        type="button"
        data-testid="pending-remote-open-allow-agents-confirm"
        disabled={busy}
        className={LINK}
        onClick={() => {
          setBusy(true);
          allowAgentsAndRelaunch(open.machineId, teamKey, open.requestId).catch((e: unknown) => {
            setBusy(false);
            toast.error(`Couldn't allow agents on ${open.label}: ${getErrorMessage(e)}`);
          });
        }}
      >
        Allow
      </button>
      <button type="button" className={LINK} onClick={() => setAsking(false)}>
        Cancel
      </button>
    </span>
  );
}

/** A workspace's remote opens with no tab yet: opening, unconfirmed (check
 *  again — never open twice), or refused. */
export function PendingRemoteOpens({
  teamKey,
  workspaceId,
}: {
  teamKey: string;
  workspaceId: string;
}) {
  const opens = usePendingRemoteOpens(teamKey, workspaceId);
  if (opens.length === 0) return null;
  return (
    <ul
      aria-label="Opening terminals"
      data-testid="pending-remote-opens"
      className="absolute top-0 inset-x-0 z-30 flex flex-col gap-1 p-2"
    >
      {opens.map((open) => (
        <li
          key={open.requestId}
          role="status"
          className="mx-auto flex max-w-xl items-center gap-2 rounded-md border border-[#30363d] bg-[#161b22] px-3 py-1.5 text-xs text-[#c9d1d9] shadow"
        >
          {open.status === "opening" && <Loader2 className="size-3.5 animate-spin" aria-hidden />}
          <span className="min-w-0 flex-1">
            {open.status === "opening"
              ? `Opening ${noun(open)} on ${open.label}…`
              : open.status === "refused"
                ? refusalText(open)
                : stalled(open)
                  ? `${noun(open, true)} on ${open.label} isn't confirmed: ${open.message}`
                  : `Not confirmed on ${open.label}: ${open.message}`}
          </span>
          {stalled(open) && (
            <>
              <button
                type="button"
                data-testid="pending-remote-open-inspect"
                className={LINK}
                onClick={() => openMachinesPanel(null)}
              >
                Inspect
              </button>
              <button
                type="button"
                data-testid="pending-remote-open-attach"
                className={LINK}
                onClick={() => recheckRemoteOpen(open.requestId)}
              >
                Attach
              </button>
            </>
          )}
          {open.status === "refused" && open.code === "agentCliMissing" && (
            <InstallCommand open={open} />
          )}
          {open.status === "refused" && open.code === "agentsNotAllowed" && (
            <AllowAgents open={open} teamKey={teamKey} />
          )}
          {open.status === "refused" && open.code === "agentSetupFailed" && (
            <button
              type="button"
              className={LINK}
              onClick={() => relaunchRefusedRemoteOpen(open.requestId)}
            >
              Try again
            </button>
          )}
          {((open.status === "unknown" && !stalled(open)) || open.retryable) && (
            <button
              type="button"
              className="text-[#58a6ff] hover:underline"
              onClick={() => recheckRemoteOpen(open.requestId)}
            >
              {open.retryable ? "Retry preparation" : "Check again"}
            </button>
          )}
          {open.status === "refused" &&
            !open.retryable &&
            ["newIntentRequired"].includes(open.code ?? "") && (
              <button
                type="button"
                className="text-[#58a6ff] hover:underline"
                onClick={() => restartRefusedRemoteOpen(open.requestId)}
              >
                Start a new launch
              </button>
            )}
          {open.status === "refused" && open.code === "terminalServerOutdated" && (
            <button
              type="button"
              data-testid="pending-remote-open-update"
              className="text-[#58a6ff] hover:underline"
              onClick={() => offerTerminalServerUpdate(open.machineId)}
            >
              Update terminal server
            </button>
          )}
          {open.status !== "opening" && (
            <button
              type="button"
              aria-label="Dismiss"
              className="text-[#8b949e] hover:text-[#e6edf3]"
              onClick={() => dismissRemoteOpen(open.requestId)}
            >
              <X className="size-3.5" />
            </button>
          )}
        </li>
      ))}
    </ul>
  );
}
