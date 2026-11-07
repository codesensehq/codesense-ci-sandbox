// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { Loader2, Server } from "lucide-react";
import { toast } from "sonner";
import { getErrorMessage } from "@/shared/lib/error-message";

import { AppButton as Button } from "@/shared/components/app-button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { SelectMenu } from "@/shared/components/select-menu";
import { ReservePanel } from "@/features/shell/components/connections-reserve";
import { CREDENTIALS_IN_KEYCHAIN } from "@/features/settings/lib/keychain-copy";
import { useCopyToClipboard } from "@/shared/hooks/use-copy-to-clipboard";
import { parseTeamKey } from "@/shared/lib/team-key";
import {
  captureMachinePassphrase,
  installTerminalServer,
  enrollPrivateMachineAccount,
  listSshSources,
  saveMachine,
  setMachineAutoConnect,
  testMachineConnection,
  type InstallTerminalServerResult,
  type MachineDraftInput,
  type MachineNextStep,
  type MachineScope,
  type MachineSignIn,
  type MachineSummary,
  type MachineTestReport,
  type MachineTestStep,
  type MachineTestStepStatus,
  type SshLocalSources,
} from "@/tauri/commands";
import type { CredentialRef } from "@/shared/types/domain";

import { formatMachineAddress, parseMachineAddress } from "../lib/machine-address";
import { retireLeftoverLegacyServer } from "../lib/machine-actions";
import { MB } from "../lib/machine-file-path";
import { TONE_CLASS, type MachineTone } from "../lib/machine-status";
import { reloadMachines } from "../stores/machines-store";

const STEP_LABEL: Record<MachineTestStep["id"], string> = {
  "private-account": "Private account",
  agentPrerequisite: "Agent runtime",
  reachable: "Reachable",
  hostKey: "Host key",
  auth: "Sign in",
  platform: "OS / arch",
  dataDir: "~/.codesense",
  terminalServer: "Terminal server",
};

const STEP_GLYPH: Record<MachineTestStepStatus, string> = {
  ok: "✓",
  needsTrust: "?",
  warn: "!",
  fail: "✕",
  blocked: "⛔",
  skipped: "–",
};

const STEP_TONE: Record<MachineTestStepStatus, MachineTone> = {
  ok: "ok",
  needsTrust: "warn",
  warn: "warn",
  fail: "error",
  blocked: "error",
  skipped: "muted",
};

const NEXT_STEP_LABEL: Record<MachineNextStep, string> = {
  "enroll-private-account": "Enroll this Unix account",
  "install-terminal-server": "Install private terminal server",
  editAddress: "Edit address",
  useJumpHost: "Use jump host",
  retry: "Retry",
  chooseKey: "Choose key",
  changeUser: "Change user",
  copyPublicKey: "Copy public key",
};

/** Next steps that change the shared address — the publishers' to take, not a
 *  member's (the locked declaration has no address / jump-host fields). */
const ADDRESS_STEPS: ReadonlySet<MachineNextStep> = new Set(["editAddress", "useJumpHost"]);

type SignInMethod = MachineSignIn["method"];
const SIGN_IN_METHODS: { value: SignInMethod; label: string }[] = [
  { value: "agent", label: "ssh-agent" },
  { value: "keyFile", label: "Key file" },
  { value: "keychainPassphrase", label: "Key + passphrase" },
];

/** Every checklist step passed — the only state Connect accepts. */
function isReadyToConnect(report: MachineTestReport | null): boolean {
  return report !== null && report.steps.every((s) => s.status === "ok");
}

type InstallState =
  | { status: "idle" | "installing" }
  | { status: "installed"; logoutWarning: string | null }
  | { status: "failed"; result: Extract<InstallTerminalServerResult, { ok: false }> };

/** The sign-in a saved binding implies: a kept passphrase, a key file, or the agent. */
function initialMethod(machine: MachineSummary | null): SignInMethod {
  if (machine?.binding?.credentialRef) return "keychainPassphrase";
  return machine?.binding?.keyPath ? "keyFile" : "agent";
}

/**
 * The Set up / Reconnect / Edit address / Install panel for a saved machine,
 * prefilled, in the Connections two-pane rhythm: Where → Sign in → a live Test
 * checklist → Install (only if needed) → Share → Connect. Every failure is
 * inline with its specific next step; a team-pin mismatch is blocked with no
 * bypass. Adding a new machine is {@link QuickAddMachine}'s one step.
 *
 * A passphrase never enters this panel: a system dialog puts it straight into
 * the keychain, and only its handle comes back.
 */
export function AddMachinePanel({
  teamKey,
  machine,
  initialMode,
  onClose,
}: {
  teamKey: string;
  machine: MachineSummary;
  /** "private-account" opens a legacy machine already switching to a private account. */
  initialMode?: "private-account";
  onClose: () => void;
}) {
  const { orgSlug, teamName: team } = parseTeamKey(teamKey);
  const decl = machine?.declaration ?? null;
  const [address, setAddress] = useState(() =>
    decl
      ? formatMachineAddress({
          username: machine?.binding?.username ?? null,
          host: decl.host,
          port: decl.port,
        })
      : "",
  );
  // Only a private account opens terminal tabs, so new machines start there.
  const [mode, setMode] = useState<"legacy" | "private-account">(
    initialMode ?? decl?.mode ?? "private-account",
  );
  const [label, setLabel] = useState(machine?.label ?? "");
  const [autoConnect, setAutoConnect] = useState(machine?.binding?.autoConnect === true);
  const [jumpHost, setJumpHost] = useState(decl?.jumpHost ?? "");
  const [showJump, setShowJump] = useState(Boolean(decl?.jumpHost));
  const [method, setMethod] = useState<SignInMethod>(() => initialMethod(machine));
  const [keyPath, setKeyPath] = useState(machine?.binding?.keyPath ?? "~/.ssh/id_ed25519");
  /** The keychain handle of the key's passphrase — never the passphrase. */
  const [passphraseRef, setPassphraseRef] = useState<CredentialRef | null>(
    machine?.binding?.credentialRef ?? null,
  );
  const [capturing, setCapturing] = useState(false);
  const keyPathRef = useRef(keyPath);
  keyPathRef.current = keyPath;
  // A trusted fingerprint belongs to ONE host: editing the address never carries
  // it over to another machine.
  const [trusted, setTrusted] = useState<{ host: string; fp: string } | null>(() =>
    decl?.pinnedHostKeyFp ? { host: decl.host, fp: decl.pinnedHostKeyFp } : null,
  );
  const [share, setShare] = useState<"personal" | "team">(
    machine?.scope?.kind === "team" ? "team" : "personal",
  );
  const [sources, setSources] = useState<SshLocalSources | null>(null);
  const [report, setReport] = useState<MachineTestReport | null>(null);
  const [testing, setTesting] = useState(false);
  const testSeq = useRef(0);
  /** Bumped only by an edit — an install's result is dropped only when the input
   *  it was for changed (a re-test meanwhile doesn't make it stale). */
  const editSeq = useRef(0);
  /** A Connect save is in flight — a second click must not save twice. */
  const busy = useRef(false);
  /** ✕ already closed the panel while a save was in flight: don't close it again. */
  const closed = useRef(false);
  const closeOnce = () => {
    if (closed.current) return;
    closed.current = true;
    onClose();
  };
  const [install, setInstall] = useState<InstallState>({ status: "idle" });
  const [showCommands, setShowCommands] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const addressRef = useRef<HTMLInputElement>(null);
  const jumpRef = useRef<HTMLInputElement>(null);
  const keyRef = useRef<HTMLInputElement>(null);
  const { handleCopy } = useCopyToClipboard();

  useEffect(() => {
    let cancelled = false;
    void listSshSources()
      .then((s) => {
        if (!cancelled) setSources(s);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const parsed = parseMachineAddress(address);
  // The jump host is an address too, and a key path is a file — neither may read
  // as an ssh option.
  const jumpOk = jumpHost.trim() === "" || parseMachineAddress(jumpHost) !== null;
  const keyPathOk = !keyPath.trim().startsWith("-");
  const trustedFp = trusted && parsed && trusted.host === parsed.host ? trusted.fp : null;
  const signIn: MachineSignIn | null =
    method === "agent"
      ? { method }
      : method === "keyFile"
        ? { method, keyPath: keyPath.trim() }
        : passphraseRef && { method, keyPath: keyPath.trim(), credentialRef: passphraseRef };
  const signInComplete =
    signIn !== null && (method === "agent" || (keyPath.trim() !== "" && keyPathOk));
  const draft = (fp: string | null): MachineDraftInput | null =>
    parsed && jumpOk && signIn && signInComplete
      ? {
          mode,
          label: label.trim() || parsed.host,
          host: parsed.host,
          port: parsed.port,
          username: parsed.username,
          jumpHost: jumpHost.trim() || null,
          signIn,
          trustedHostKeyFp: fp,
        }
      : null;
  const scope: MachineScope =
    share === "team" && team ? { kind: "team", orgSlug, team } : { kind: "personal" };
  const agentKey = sources?.agentKeys[0] ?? null;

  /** Any edit to Where / Sign in invalidates the last test — including one still
   *  in flight (its result would describe the old input). */
  const dirty = () => {
    testSeq.current += 1;
    editSeq.current += 1;
    setTesting(false);
    setReport(null);
    setInstall({ status: "idle" });
  };

  const runTest = async (fp: string | null = trustedFp) => {
    const input = draft(fp);
    if (!input) return;
    const seq = ++testSeq.current;
    setTesting(true);
    try {
      const result = await testMachineConnection(machine?.machineId ?? null, input);
      if (seq === testSeq.current) setReport(result);
    } catch (e) {
      if (seq === testSeq.current)
        toast.error(`Couldn't test ${input.host}: ${getErrorMessage(e)}`);
    } finally {
      if (seq === testSeq.current) setTesting(false);
    }
  };

  const runInstall = async () => {
    const input = draft(trustedFp);
    if (!input) return;
    // An edit while installing (dirty() bumps the sequence) makes this result
    // about the old input — drop it rather than re-test or report on it.
    const seq = editSeq.current;
    setInstall({ status: "installing" });
    let result: InstallTerminalServerResult;
    try {
      result = await installTerminalServer(machine?.machineId ?? null, input);
    } catch (e) {
      if (seq !== editSeq.current) return;
      setInstall({ status: "idle" });
      toast.error(`Couldn't install on ${input.host}: ${getErrorMessage(e)}`);
      return;
    }
    if (seq !== editSeq.current) return;
    if (result.ok) {
      setInstall({ status: "installed", logoutWarning: result.logoutWarning });
      await runTest();
    } else {
      setInstall({ status: "failed", result });
    }
  };

  /** A system dialog asks for the passphrase and keeps it in the keychain; only
   *  its handle comes back. Cancelling changes nothing. */
  const savePassphrase = async () => {
    setCapturing(true);
    try {
      const forKey = keyPath.trim();
      const handle = await captureMachinePassphrase(forKey);
      // The key path changed while the dialog was open: this passphrase is not its.
      if (handle && keyPathRef.current.trim() === forKey) {
        setPassphraseRef(handle);
        dirty();
      }
    } catch (e) {
      toast.error(`Couldn't keep the passphrase: ${getErrorMessage(e)}`);
    } finally {
      setCapturing(false);
    }
  };

  const trust = (fp: string) => {
    if (parsed) setTrusted({ host: parsed.host, fp });
    void runTest(fp);
  };

  const onNextStep = (step: MachineNextStep) => {
    switch (step) {
      case "enroll-private-account": {
        const input = draft(trustedFp);
        if (!input) return;
        const seq = editSeq.current;
        setTesting(true);
        void enrollPrivateMachineAccount(machine?.machineId ?? null, input)
          .then((report) => {
            if (seq === editSeq.current) setReport(report);
          })
          .catch((error) => {
            if (seq === editSeq.current) toast.error(getErrorMessage(error));
          })
          .finally(() => {
            if (seq === editSeq.current) setTesting(false);
          });
        return;
      }
      case "install-terminal-server":
        void runInstall();
        return;
      case "editAddress":
      case "changeUser":
        addressRef.current?.focus();
        return;
      case "useJumpHost":
        setShowJump(true);
        requestAnimationFrame(() => jumpRef.current?.focus());
        return;
      case "retry":
        void runTest();
        return;
      case "chooseKey":
        setMethod("keyFile");
        dirty();
        requestAnimationFrame(() => keyRef.current?.focus());
        return;
      case "copyPublicKey":
        if (agentKey) handleCopy(agentKey.publicKey);
        else toast.error("No ssh-agent key found — add one with ssh-add, then copy it.");
        return;
    }
  };

  // Only the CURRENT result counts: a re-test or install in flight may change it.
  const canConnect = isReadyToConnect(report) && !testing && install.status !== "installing";
  const connect = async () => {
    const input = draft(trustedFp);
    if (!input || !canConnect || busy.current) return;
    busy.current = true;
    setConnecting(true);
    try {
      const saved = await saveMachine(machine?.machineId ?? null, input, scope, true);
      if (autoConnect !== (saved.binding?.autoConnect === true))
        await setMachineAutoConnect(saved.machineId, autoConnect);
      await reloadMachines(teamKey);
      if (machine?.declaration?.mode === "legacy" && saved.declaration?.mode === "private-account")
        void retireLeftoverLegacyServer(saved.machineId, saved.label);
      // RM-3 verifies and enrolls the machine. Connection lifecycle starts in
      // RM-4; selecting here would invoke its draft-only connect command.
      toast.success(machine ? `Saved ${saved.label}` : `Added ${saved.label}`);
      closeOnce();
    } catch (e) {
      toast.error(`Couldn't connect: ${getErrorMessage(e)}`);
      busy.current = false;
    } finally {
      setConnecting(false);
    }
  };

  const hostKeyStep = report?.steps.find((s) => s.id === "hostKey");
  const blocked = hostKeyStep?.status === "blocked";
  const canPickShare = !machine || machine.canPublish;
  // A teammate's shared declaration (address, jump host, label, pinned key) is the
  // publishers' to change — a member only binds their own user + credential.
  const lockedDecl = decl !== null && !machine?.canPublish;

  return (
    <ReservePanel
      testId="add-machine-panel"
      icon={<Server size={16} aria-hidden="true" />}
      title={machine.binding ? `Edit ${machine.label}` : `Set up ${machine.label}`}
      subtitle={
        machine?.binding
          ? `Change how CodeSense reaches ${machine.label}, then test and reconnect.`
          : "Connect a machine over SSH and run CodeSense's terminal server on it."
      }
      closeLabel="Close add-machine panel"
      onClose={closeOnce}
      footer={
        <>
          <Button variant="outline" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            size="sm"
            data-testid="add-machine-connect"
            disabled={!canConnect || connecting}
            onClick={() => void connect()}
          >
            {connecting ? "Connecting…" : "Connect"}
          </Button>
        </>
      }
    >
      <div className="mt-4 flex flex-col gap-4">
        <Section n={1} title="Where">
          {decl?.mode === "legacy" && (
            <label className="text-xs">
              Session privacy
              <select
                aria-label="Session privacy"
                value={mode}
                // Its publisher may switch a legacy machine to a private account in place; never back.
                disabled={!machine.canPublish}
                onChange={(event) => {
                  setMode(event.target.value as "legacy" | "private-account");
                  dirty();
                }}
              >
                <option value="private-account">Private Unix account</option>
                <option value="legacy">Legacy shared account (no terminal tabs)</option>
              </select>
            </label>
          )}
          {mode === "private-account" ? (
            <p className="text-xs text-muted-foreground">
              {decl?.mode === "legacy"
                ? "Switching keeps this machine's name, host key and settings. Enroll your Unix account, then install its terminal server."
                : "Use your own Unix account. Enroll explicitly after verifying the SSH host key."}
            </p>
          ) : (
            <p className="text-xs text-muted-foreground">
              Legacy machines connect but cannot open terminal tabs. Choose a private account to use
              terminals.
            </p>
          )}
          {!lockedDecl && sources && sources.configHosts.length > 0 && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="machine-ssh-config">From ~/.ssh/config</Label>
              <SelectMenu
                id="machine-ssh-config"
                ariaLabel="Pick a host from ~/.ssh/config"
                value=""
                options={[
                  { value: "", label: "Pick a host…" },
                  ...sources.configHosts.map((h) => ({ value: h.alias, label: h.alias })),
                ]}
                onChange={(alias) => {
                  const h = sources.configHosts.find((c) => c.alias === alias);
                  if (!h) return;
                  setAddress(
                    formatMachineAddress({ username: h.user, host: h.hostName, port: h.port }),
                  );
                  setLabel(h.alias);
                  dirty();
                }}
              />
            </div>
          )}
          {lockedDecl && decl ? (
            <div className="flex flex-col gap-1.5">
              <p data-testid="machine-locked-address" className="text-xs text-muted-foreground">
                {formatMachineAddress({ username: null, host: decl.host, port: decl.port })}
                {decl.jumpHost ? ` via ${decl.jumpHost}` : ""} — set by whoever manages this machine
                for the team.
              </p>
              <Label htmlFor="machine-address">Your user on it</Label>
              <Input
                id="machine-address"
                ref={addressRef}
                placeholder="username"
                value={parsed?.username ?? ""}
                onChange={(e) => {
                  setAddress(
                    formatMachineAddress({
                      username: e.target.value.trim() || null,
                      host: decl.host,
                      port: decl.port,
                    }),
                  );
                  dirty();
                }}
              />
            </div>
          ) : (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="machine-address">Address</Label>
              <Input
                id="machine-address"
                ref={addressRef}
                placeholder="user@host:port"
                value={address}
                onChange={(e) => {
                  setAddress(e.target.value);
                  dirty();
                }}
              />
              {address.trim() && !parsed && (
                <p className="text-[11px] text-destructive">Use user@host or user@host:port.</p>
              )}
            </div>
          )}
          {lockedDecl ? null : showJump ? (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="machine-jump">Jump host (optional)</Label>
              <Input
                id="machine-jump"
                ref={jumpRef}
                placeholder="bastion"
                value={jumpHost}
                onChange={(e) => {
                  setJumpHost(e.target.value);
                  dirty();
                }}
              />
              {!jumpOk && (
                <p className="text-[11px] text-destructive">Use host or user@host:port.</p>
              )}
            </div>
          ) : (
            <button
              type="button"
              onClick={() => setShowJump(true)}
              className="self-start text-xs text-muted-foreground hover:text-foreground"
            >
              + Jump host
            </button>
          )}
          {!lockedDecl && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="machine-label">Label</Label>
              <Input
                id="machine-label"
                placeholder={parsed?.host ?? "devbox-01"}
                value={label}
                onChange={(e) => setLabel(e.target.value)}
              />
            </div>
          )}
        </Section>

        <Section n={2} title="Sign in">
          <div
            role="radiogroup"
            aria-label="Sign-in method"
            className="flex rounded-md border border-border p-0.5"
          >
            {SIGN_IN_METHODS.map((m) => (
              <button
                key={m.value}
                type="button"
                role="radio"
                aria-checked={method === m.value}
                data-testid={`machine-signin-${m.value}`}
                onClick={() => {
                  setMethod(m.value);
                  dirty();
                }}
                className={cn(
                  "flex-1 rounded px-2 py-1 text-xs transition-colors",
                  method === m.value
                    ? "bg-accent font-medium text-accent-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {m.label}
              </button>
            ))}
          </div>
          {method === "agent" ? (
            <p className="text-xs text-muted-foreground">
              {agentKey
                ? `✓ Using ssh-agent key ${agentKey.keyType} ${agentKey.fingerprint}`
                : "No ssh-agent key found — your ~/.ssh/config identity is used."}
            </p>
          ) : (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="machine-key">Key file</Label>
              <Input
                id="machine-key"
                ref={keyRef}
                value={keyPath}
                onChange={(e) => {
                  setKeyPath(e.target.value);
                  // A kept passphrase belongs to the key it unlocked.
                  setPassphraseRef(null);
                  dirty();
                }}
              />
              {!keyPathOk && (
                <p className="text-[11px] text-destructive">
                  Use a path to the key file, like ~/.ssh/id_ed25519.
                </p>
              )}
            </div>
          )}
          {method === "keychainPassphrase" && (
            <div className="flex flex-col gap-1.5">
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  data-testid="machine-save-passphrase"
                  disabled={capturing || keyPath.trim() === "" || !keyPathOk}
                  onClick={() => void savePassphrase()}
                >
                  {capturing
                    ? "Waiting for the passphrase…"
                    : passphraseRef
                      ? "Change passphrase…"
                      : "Save passphrase to Keychain…"}
                </Button>
                {passphraseRef && !capturing && (
                  <span
                    data-testid="machine-passphrase-saved"
                    className="text-xs text-muted-foreground"
                  >
                    ✓ Saved in your keychain
                  </span>
                )}
              </div>
              <p className="text-[11px] text-muted-foreground">
                macOS asks for it in its own dialog. CodeSense keeps it in your keychain and never
                shows or sends it.
              </p>
            </div>
          )}
        </Section>

        <Section n={3} title="Test">
          <Button
            variant="outline"
            size="sm"
            className="self-start"
            data-testid="add-machine-test"
            disabled={!draft(trustedFp) || testing || install.status === "installing"}
            onClick={() => void runTest()}
          >
            {testing ? "Testing…" : report ? "Test again" : "Test connection"}
          </Button>
          {testing && (
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 size={12} className="animate-spin" aria-hidden="true" />
              Checking {parsed?.host}…
            </p>
          )}
          {report && !testing && (
            <ul data-testid="add-machine-checklist" className="flex flex-col gap-1">
              {report.steps.map((step) => (
                <ChecklistRow
                  key={step.id}
                  step={step}
                  nextSteps={step.nextSteps.filter((n) => !lockedDecl || !ADDRESS_STEPS.has(n))}
                  onNextStep={onNextStep}
                  onTrust={trust}
                />
              ))}
            </ul>
          )}
          {blocked && (
            <p data-testid="add-machine-blocked" className="text-xs text-destructive">
              Blocked. This key doesn't match the one your team pinned, and there's no bypass. Ask
              whoever manages this machine to confirm it.
            </p>
          )}
          {install.status === "installed" && install.logoutWarning && (
            <p
              role="note"
              data-testid="add-machine-logout-warning"
              className={cn("text-[11px]", TONE_CLASS.warn)}
            >
              {install.logoutWarning}
            </p>
          )}
          {report?.fileViewOnly && (
            <p className="text-xs text-muted-foreground">
              This OS can't run the terminal server yet. Close to keep it as a draft — file view for
              it comes later.
            </p>
          )}
        </Section>

        {report?.installPlan && (
          <Section n={4} title="Install">
            <Button
              size="sm"
              variant="outline"
              className="self-start"
              data-testid="add-machine-install"
              disabled={install.status === "installing"}
              onClick={() => void runInstall()}
            >
              {install.status === "installing"
                ? "Installing…"
                : `Install terminal server (${Math.round(report.installPlan.sizeBytes / MB)} MB to ${report.installPlan.targetDir}, no sudo)`}
            </Button>
            {install.status === "failed" && (
              <div
                role="alert"
                data-testid="add-machine-install-error"
                className="flex items-center gap-2 text-xs text-destructive"
              >
                <span>{install.result.message}</span>
                {install.result.code === "interrupted" && (
                  <button
                    type="button"
                    data-testid="add-machine-install-resume"
                    onClick={() => void runInstall()}
                    className="rounded border border-border px-1.5 py-0.5 text-foreground hover:bg-accent"
                  >
                    Resume
                  </button>
                )}
              </div>
            )}
            <button
              type="button"
              onClick={() => setShowCommands((v) => !v)}
              className="self-start text-xs text-muted-foreground underline-offset-2 hover:underline"
            >
              {showCommands ? "Hide commands" : "Show commands"}
            </button>
            {showCommands && (
              <pre className="overflow-x-auto rounded bg-muted p-2 text-[11px]">
                {report.installPlan.commands.join("\n")}
              </pre>
            )}
          </Section>
        )}

        <Section n={report?.installPlan ? 5 : 4} title="Share">
          {canPickShare && team ? (
            <div
              role="radiogroup"
              aria-label="Who sees this machine"
              className="flex flex-col gap-1"
            >
              {(
                [
                  ["personal", "Only me"],
                  ["team", `Share with ${team}`],
                ] as const
              ).map(([value, text]) => (
                <label key={value} className="flex items-center gap-2 text-xs">
                  <input
                    type="radio"
                    name="machine-share"
                    data-testid={`machine-share-${value}`}
                    checked={share === value}
                    onChange={() => setShare(value)}
                  />
                  {text}
                </label>
              ))}
              {share === "team" && (
                <p className="text-[11px] text-muted-foreground">
                  Teammates see the address and the pinned key; they sign in with their own keys.
                </p>
              )}
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">
              {machine?.scope?.kind === "team"
                ? `Shared with ${machine.scope.team} — you sign in with your own key.`
                : "Just you."}
            </p>
          )}
        </Section>

        <label className="flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            checked={autoConnect}
            onChange={(event) => setAutoConnect(event.target.checked)}
            disabled={connecting}
            aria-describedby="machine-auto-connect-help"
          />
          Reconnect in background
        </label>
        <p id="machine-auto-connect-help" className="text-[11px] text-muted-foreground">
          Your setting. Machines with open tabs reconnect when CodeSense opens either way.
          Background reconnect never grants trust or prompts for credentials, so a key with a
          passphrase connects only when you pick the machine.
        </p>
        <p className="text-[11px] text-muted-foreground">{CREDENTIALS_IN_KEYCHAIN}.</p>
      </div>
    </ReservePanel>
  );
}

function Section({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
  return (
    <section className="flex flex-col gap-2">
      <h4 className="text-xs font-semibold">
        <span className="mr-1.5 text-muted-foreground">{n}</span>
        {title}
      </h4>
      {children}
    </section>
  );
}

function ChecklistRow({
  step,
  nextSteps,
  onNextStep,
  onTrust,
}: {
  step: MachineTestStep;
  /** The step's next steps this user can actually take. */
  nextSteps: MachineNextStep[];
  onNextStep: (step: MachineNextStep) => void;
  onTrust: (fingerprint: string) => void;
}) {
  return (
    <li data-testid={`machine-check-${step.id}`} className="flex flex-col gap-1 text-xs">
      <div className="flex items-start gap-1.5">
        <span
          className={cn("w-3 shrink-0 text-center", TONE_CLASS[STEP_TONE[step.status]])}
          aria-hidden="true"
        >
          {STEP_GLYPH[step.status]}
        </span>
        <span className="w-24 shrink-0 font-medium">{STEP_LABEL[step.id]}</span>
        <span className={cn("min-w-0", step.status === "skipped" && "text-muted-foreground")}>
          {step.status === "skipped" ? "Not checked yet" : step.detail}
        </span>
      </div>
      {step.status === "needsTrust" && step.fingerprint && (
        <div className="ml-[7.5rem] flex items-center gap-2">
          <code className="rounded bg-muted px-1 text-[11px]">{step.fingerprint}</code>
          <button
            type="button"
            data-testid="machine-trust-key"
            onClick={() => onTrust(step.fingerprint!)}
            className="rounded border border-border px-1.5 py-0.5 hover:bg-accent"
          >
            Trust
          </button>
        </div>
      )}
      {nextSteps.length > 0 && (
        <div className="ml-[7.5rem] flex flex-wrap gap-1">
          {nextSteps.map((next) => (
            <button
              key={next}
              type="button"
              data-testid={`machine-next-${next}`}
              onClick={() => onNextStep(next)}
              className="rounded border border-border px-1.5 py-0.5 hover:bg-accent"
            >
              {NEXT_STEP_LABEL[next]}
            </button>
          ))}
        </div>
      )}
    </li>
  );
}
