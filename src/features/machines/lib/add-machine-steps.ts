// SPDX-License-Identifier: MIT
import type { MachineNextStep, MachineTestReport, MachineTestStep } from "@/tauri/commands";

/**
 * The one-step add flow's reading of what the backend said. The backend keeps
 * its explicit steps (test, trust, enroll, install, save, connect) for agents;
 * the add flow composes them and only needs to know what comes next.
 */

/** What a test or enroll report asks for next. */
export type ReportNext =
  | { kind: "trust"; fingerprint: string }
  | { kind: "enroll" }
  | { kind: "install" }
  | { kind: "ready" }
  | { kind: "fail"; message: string; nextSteps: MachineNextStep[]; blocked: boolean };

/** What `report` asks for next: a fingerprint to trust, an enrollment, an
 *  install, a failure with its fix, or nothing (ready to save). */
export function nextFromReport(report: MachineTestReport): ReportNext {
  const blocked = report.steps.find((s) => s.status === "blocked");
  if (blocked)
    return {
      kind: "fail",
      message:
        "This machine's host key doesn't match the one pinned for your team. There's no way around that here: ask whoever manages the machine to confirm it.",
      nextSteps: [],
      blocked: true,
    };
  const failed = report.steps.find((s) => s.status === "fail");
  if (failed) return failure(failed);
  const trust = report.steps.find((s) => s.status === "needsTrust" && s.fingerprint);
  if (trust?.fingerprint) return { kind: "trust", fingerprint: trust.fingerprint };
  if (report.fileViewOnly) {
    const platform = report.steps.find((s) => s.id === "platform");
    return {
      kind: "fail",
      message: platform?.detail ?? "This machine's OS can't run the terminal server yet.",
      nextSteps: [],
      blocked: false,
    };
  }
  const asks = (step: MachineNextStep) => report.steps.some((s) => s.nextSteps.includes(step));
  if (asks("enroll-private-account")) return { kind: "enroll" };
  if (asks("install-terminal-server") || report.installPlan) return { kind: "install" };
  const warned = report.steps.find((s) => s.status === "warn" && s.id !== "agentPrerequisite");
  if (warned) return failure(warned);
  return { kind: "ready" };
}

function failure(step: MachineTestStep): ReportNext {
  return { kind: "fail", message: step.detail, nextSteps: step.nextSteps, blocked: false };
}

/** A way forward offered under a failed step. */
export type FixAction =
  | "editAddress"
  | "useJumpHost"
  | "chooseKey"
  | "copyPublicKey"
  | "checkAgain"
  | "retry";

export interface FailureFix {
  /** One sentence: what to do about it. */
  fix: string;
  actions: FixAction[];
}

const NEXT_TO_FIX: Partial<Record<MachineNextStep, FixAction>> = {
  editAddress: "editAddress",
  useJumpHost: "useJumpHost",
  changeUser: "editAddress",
  chooseKey: "chooseKey",
  copyPublicKey: "copyPublicKey",
  retry: "retry",
};

/** The next step for a failure, from what the backend said (`message`) and
 *  the next steps it offered. Every failure gets one. */
export function fixFor(message: string, nextSteps: readonly MachineNextStep[] = []): FailureFix {
  const offered = [...new Set(nextSteps.flatMap((n) => (NEXT_TO_FIX[n] ? [NEXT_TO_FIX[n]!] : [])))];
  const text = message.toLowerCase();
  if (/permission denied|publickey|key rejected|authentication|auth fail|too many auth/.test(text))
    return {
      fix: "The machine refused your key. Add your public key to ~/.ssh/authorized_keys for this user on the machine (most cloud consoles do this when you create it), or sign in with another key.",
      actions: ["copyPublicKey", "chooseKey", "editAddress", "retry"],
    };
  // Private setup's own refusals (private_account.rs). Matched before the connection
  // advice below, which would point the wrong way.
  if (/already set up for a different codesense identity/.test(text))
    return {
      fix: "This login on the machine is set up for another CodeSense profile, or for this one before its identity changed. One login holds one identity. Use another user on the machine.",
      actions: ["editAddress"],
    };
  if (/private account setup timed out/.test(text))
    return {
      fix: "Setup did not finish in time. A slow or stalled connection to the machine is the usual cause. Try again, or check the address.",
      actions: ["retry", "editAddress"],
    };
  if (/could not resolve|name or service not known|nodename nor servname|unresolved/.test(text))
    return {
      fix: "That name doesn't resolve. Check the address for a typo, or use the machine's IP.",
      actions: ["editAddress", "retry"],
    };
  if (
    /timed out|timeout|no response|no route|unreachable|connection refused|couldn't reach|network is down/.test(
      text,
    )
  )
    return {
      fix: "Check the address, that the machine is running, and that its firewall allows SSH (port 22). If it's only reachable through another host, add a jump host.",
      actions: ["editAddress", "useJumpHost", "retry"],
    };
  if (/host key|different key/.test(text))
    return {
      fix: "The machine presents a different key than the one you trusted: it may have been rebuilt, or the address now reaches another machine. Check again to see the key it presents now, or fix the address.",
      actions: ["checkAgain", "editAddress"],
    };
  if (/disk|no space|not writable|read-only file system/.test(text))
    return {
      fix: "Free some space in your home folder on the machine, then try again.",
      actions: ["retry"],
    };
  if (/interrupted|connection closed|broken pipe|reset by peer/.test(text))
    return {
      fix: "The connection dropped. Try again: setup picks up where it stopped.",
      actions: ["retry"],
    };
  return {
    fix: "Try again. If it keeps failing, check that you can SSH to the machine from Terminal.",
    actions: offered.length > 0 ? offered : ["retry"],
  };
}
