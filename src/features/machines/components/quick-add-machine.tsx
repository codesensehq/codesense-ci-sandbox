// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { Loader2, Server } from "lucide-react";
import { toast } from "sonner";

import { AppButton as Button } from "@/shared/components/app-button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { ReservePanel } from "@/features/shell/components/connections-reserve";
import { CREDENTIALS_IN_KEYCHAIN } from "@/features/settings/lib/keychain-copy";
import { useCopyToClipboard } from "@/shared/hooks/use-copy-to-clipboard";
import { getErrorMessage } from "@/shared/lib/error-message";
import { activeWorkspaceIdForTeam, parseTeamKey } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  captureMachinePassphrase,
  enrollPrivateMachineAccount,
  installTerminalServer,
  listSshSources,
  saveMachine,
  setMachineAutoConnect,
  testMachineConnection,
  type MachineDraftInput,
  type MachineTestReport,
  type MachineScope,
  type MachineSignIn,
  type SshAgentKey,
  type SshConfigHost,
  type SshLocalSources,
} from "@/tauri/commands";
import type { CredentialRef } from "@/shared/types/domain";

import { formatMachineAddress, parseMachineAddress } from "../lib/machine-address";
import { connectMachineFor, openShellOn } from "../lib/machine-actions";
import {
  fixFor,
  nextFromReport,
  type FailureFix,
  type FixAction,
  type ReportNext,
} from "../lib/add-machine-steps";
import { TONE_CLASS } from "../lib/machine-status";
import { reloadMachines } from "../stores/machines-store";

/** Runtime warnings remain visible while allowing the machine to run other agents. */
function checkedReport(report: MachineTestReport): ReportNext {
  for (const step of report.steps) {
    if (step.id === "agentPrerequisite" && step.status === "warn") toast.warning(step.detail);
  }
  return nextFromReport(report);
}

type SignInMethod = MachineSignIn["method"];
const SIGN_IN_METHODS: { value: SignInMethod; label: string }[] = [
  { value: "agent", label: "ssh-agent" },
  { value: "keyFile", label: "Key file" },
  { value: "keychainPassphrase", label: "Key + passphrase" },
];

/** The steps Trust & connect runs, in order. */
type StepId = "trust" | "signIn" | "enroll" | "install" | "save" | "connect" | "shell";
const STEP_ORDER: StepId[] = ["trust", "signIn", "enroll", "install", "save", "connect", "shell"];

type StepState =
  | { status: "pending" }
  | { status: "running" }
  | { status: "done"; note?: string }
  | { status: "failed"; message: string; fix: FailureFix; resumable?: boolean };

type Phase =
  | { kind: "form" }
  | { kind: "checking" }
  | { kind: "confirm"; fingerprint: string }
  | { kind: "formError"; message: string; fix: FailureFix }
  | { kind: "running" };

/** A typed address, or an ~/.ssh/config alias resolved to its host. */
interface Target {
  username: string | null;
  host: string;
  port: number;
  /** The alias it came from (its name becomes the label), with its settings. */
  alias: SshConfigHost | null;
}

function resolveTarget(typed: string, sources: SshLocalSources | null): Target | null {
  const raw = typed.trim();
  const at = raw.lastIndexOf("@");
  const user = at > 0 ? raw.slice(0, at) : null;
  const name = at > 0 ? raw.slice(at + 1) : raw;
  const alias = sources?.configHosts.find((h) => h.alias === name);
  // A host reached through a ProxyCommand keeps its alias, so each later
  // `ssh` matches the config's Host block and runs the command.
  if (alias)
    return {
      username: user ?? alias.user,
      host: alias.proxyCommand ? alias.alias : alias.hostName,
      port: alias.port,
      alias,
    };
  const parsed = parseMachineAddress(raw);
  return parsed && { ...parsed, alias: null };
}

/** A new machine's id, minted once per add so a retried save updates the
 *  machine it may already have written instead of adding a second one.
 *  The shape the backend mints: a slug of the name and six hex digits. */
export function mintMachineId(name: string): string {
  const slug =
    name
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 32)
      .replace(/-+$/, "") || "machine";
  const hex = Array.from(crypto.getRandomValues(new Uint8Array(3)), (b) =>
    b.toString(16).padStart(2, "0"),
  ).join("");
  return `${slug}-${hex}`;
}

const HOST_KEY_CHANGED = "The machine now presents a different host key than the one you trusted.";

/**
 * Add a remote machine in one step: its address (and, only if needed, how to
 * sign in), one look at its host key, then **Trust & connect** checks the
 * sign-in, sets up your private account on it, installs its terminal server,
 * saves it, connects, and opens a shell in your home folder — one checklist,
 * each failure with its fix. Closing before it is saved keeps what was typed
 * as a `Needs setup` row; a passphrase never enters this panel.
 */
export function QuickAddMachine({ teamKey, onClose }: { teamKey: string; onClose: () => void }) {
  const { orgSlug, teamName: team } = parseTeamKey(teamKey);
  const [address, setAddress] = useState("");
  const [sources, setSources] = useState<SshLocalSources | null>(null);
  const [showMore, setShowMore] = useState(false);
  const [label, setLabel] = useState("");
  const [jumpHost, setJumpHost] = useState("");
  const [method, setMethod] = useState<SignInMethod>("agent");
  const [keyPath, setKeyPath] = useState("~/.ssh/id_ed25519");
  const [passphraseRef, setPassphraseRef] = useState<CredentialRef | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [share, setShare] = useState<"personal" | "team">("personal");
  /** Null until the person changes it: personal machines connect at launch,
   *  team machines when picked. */
  const [autoConnectChoice, setAutoConnectChoice] = useState<boolean | null>(null);
  /** The agent key to sign in with (by fingerprint); null = the first. */
  const [agentFp, setAgentFp] = useState<string | null>(null);
  const [phase, setPhase] = useState<Phase>({ kind: "form" });
  const [steps, setSteps] = useState<Record<StepId, StepState>>(() => freshSteps());
  const addressRef = useRef<HTMLInputElement>(null);
  const jumpRef = useRef<HTMLInputElement>(null);
  const keyRef = useRef<HTMLInputElement>(null);
  const keyPathRef = useRef(keyPath);
  keyPathRef.current = keyPath;
  /** The fingerprint the person trusted, for the host it was shown for. */
  const trusted = useRef<{ host: string; fp: string } | null>(null);
  /** What a run already learned, so Retry resumes instead of repeating. */
  const progress = useRef<{
    next: ReportNext | null;
    savedId: string | null;
    saveTried: boolean;
  }>({ next: null, savedId: null, saveTried: false });
  /** Bumped by each run and by closing: a superseded run stops writing. */
  const runSeq = useRef(0);
  const closed = useRef(false);
  /** The id this add saves under, once minted. */
  const machineId = useRef<string | null>(null);
  /** A fingerprint was shown: only then is there something worth a draft. */
  const fingerprintSeen = useRef(false);
  /** The alias whose settings the form last took. */
  const appliedAlias = useRef<string | null>(null);
  const { handleCopy } = useCopyToClipboard();

  useEffect(() => {
    let cancelled = false;
    void listSshSources()
      .then((s) => {
        if (!cancelled) setSources(s);
      })
      .catch(() => {});
    requestAnimationFrame(() => addressRef.current?.focus());
    return () => {
      cancelled = true;
    };
  }, []);

  const target = resolveTarget(address, sources);
  const agentKeys = sources?.agentKeys ?? [];
  const agentKey: SshAgentKey | null =
    agentKeys.find((k) => k.fingerprint === agentFp) ?? agentKeys[0] ?? null;
  const proxyCommand = target?.alias?.proxyCommand ?? null;
  const keyPathOk = !keyPath.trim().startsWith("-");
  const jumpOk = jumpHost.trim() === "" || parseMachineAddress(jumpHost) !== null;
  // An agent key signs in through a public key file naming only it, so the
  // machine is offered that one key and no other the agent holds.
  const signIn: MachineSignIn | null =
    method === "agent"
      ? agentKey?.identityFile
        ? { method: "keyFile", keyPath: agentKey.identityFile }
        : { method }
      : method === "keyFile"
        ? keyPath.trim() && keyPathOk
          ? { method, keyPath: keyPath.trim() }
          : null
        : passphraseRef && keyPath.trim() && keyPathOk
          ? { method, keyPath: keyPath.trim(), credentialRef: passphraseRef }
          : null;
  const scope: MachineScope =
    share === "team" && team ? { kind: "team", orgSlug, team } : { kind: "personal" };
  const passphraseMachine = method === "keychainPassphrase";
  const autoConnect = !passphraseMachine && (autoConnectChoice ?? scope.kind === "personal");

  const draft = (fp: string | null): MachineDraftInput | null =>
    target && jumpOk && signIn
      ? {
          mode: "private-account",
          label: label.trim() || target.alias?.alias || target.host,
          host: target.host,
          port: target.port,
          username: target.username,
          jumpHost: jumpHost.trim() || null,
          signIn,
          trustedHostKeyFp: fp,
        }
      : null;

  const closeOnce = () => {
    if (closed.current) return;
    closed.current = true;
    runSeq.current += 1;
    onClose();
  };

  /** Any edit to where or how invalidates what a run learned. */
  const edited = () => {
    runSeq.current += 1;
    // A save already tried may have written the machine: keep its id.
    const saveTried = progress.current.saveTried;
    if (!saveTried) machineId.current = null;
    progress.current = { next: null, savedId: null, saveTried };
    setSteps(freshSteps());
    if (phase.kind !== "form") setPhase({ kind: "form" });
  };

  const setStep = (id: StepId, state: StepState) => setSteps((s) => ({ ...s, [id]: state }));

  /** This add's machine id, minted on first use. */
  const idFor = (name: string) => (machineId.current ??= mintMachineId(name));

  /** An ~/.ssh/config host brings its jump host and key along. */
  const takeAlias = (typed: string) => {
    const alias = resolveTarget(typed, sources)?.alias ?? null;
    if ((alias?.alias ?? null) === appliedAlias.current) return;
    const left = sources?.configHosts.find((h) => h.alias === appliedAlias.current);
    appliedAlias.current = alias?.alias ?? null;
    // Settings the previous alias brought leave with it; the person's own stay.
    if (left?.proxyJump && jumpHost === left.proxyJump) setJumpHost("");
    if (left?.identityFile && method === "keyFile" && keyPath === left.identityFile)
      setMethod("agent");
    if (!alias) return;
    setJumpHost(alias.proxyJump ?? "");
    if (alias.identityFile) {
      setMethod("keyFile");
      setKeyPath(alias.identityFile);
      setPassphraseRef(null);
    }
  };

  /** Continue: look at the machine's host key (nothing is written). */
  const check = async () => {
    const input = draft(null);
    if (!input || phase.kind === "checking") return;
    const seq = ++runSeq.current;
    setPhase({ kind: "checking" });
    try {
      const next = checkedReport(await testMachineConnection(null, input));
      if (seq !== runSeq.current) return;
      if (next.kind !== "fail") fingerprintSeen.current = true;
      if (next.kind === "trust") setPhase({ kind: "confirm", fingerprint: next.fingerprint });
      else if (next.kind === "fail")
        setPhase({
          kind: "formError",
          message: next.message,
          fix: next.blocked ? { fix: "", actions: [] } : fixFor(next.message, next.nextSteps),
        });
      // A host whose key is already pinned needs no confirmation.
      else void trustAndConnect(null);
    } catch (e) {
      if (seq !== runSeq.current) return;
      const message = getErrorMessage(e);
      setPhase({ kind: "formError", message, fix: fixFor(message) });
    }
  };

  /** Trust & connect: every remaining step, resuming after the last one done. */
  const trustAndConnect = async (fingerprint: string | null) => {
    if (!target) return;
    if (fingerprint) trusted.current = { host: target.host, fp: fingerprint };
    const fp = trusted.current?.host === target.host ? trusted.current.fp : null;
    const input = draft(fp);
    if (!input) return;
    const seq = ++runSeq.current;
    const live = () => seq === runSeq.current;
    setPhase({ kind: "running" });
    setSteps((s) => {
      const next = { ...s };
      for (const id of STEP_ORDER) if (next[id].status !== "done") next[id] = { status: "pending" };
      next.trust = { status: "done", note: fp ?? "already trusted" };
      return next;
    });
    let current: StepId = "signIn";
    const fail = (message: string, nextSteps: ReportNext | null = null, resumable = false) => {
      if (!live()) return;
      const offered = nextSteps?.kind === "fail" ? nextSteps.nextSteps : [];
      const blocked = nextSteps?.kind === "fail" && nextSteps.blocked;
      const fix = blocked ? { fix: "", actions: [] } : fixFor(message, offered);
      setStep(current, {
        status: "failed",
        message,
        // Once saved, the address is the machine's: fix it from its row, not here.
        fix: progress.current.savedId
          ? {
              fix: `${fix.fix} It's saved, so you can also fix it later from its row in Machines.`,
              actions: fix.actions.filter((a) => a === "retry" || a === "copyPublicKey"),
            }
          : fix,
        resumable,
      });
    };
    try {
      const state = progress.current;
      if (!state.savedId) {
        // Sign in and look at the account.
        let next = state.next;
        if (!next || next.kind === "fail" || next.kind === "trust") {
          current = "signIn";
          setStep("signIn", { status: "running" });
          next = checkedReport(await testMachineConnection(null, input));
          if (!live()) return;
          if (next.kind === "fail" || next.kind === "trust") {
            fail(next.kind === "fail" ? next.message : HOST_KEY_CHANGED, next);
            return;
          }
          state.next = next;
          setStep("signIn", { status: "done", note: `as ${input.username ?? "your SSH user"}` });
        }

        current = "enroll";
        if (next.kind === "enroll") {
          setStep("enroll", { status: "running" });
          next = checkedReport(await enrollPrivateMachineAccount(null, input));
          if (!live()) return;
          if (next.kind === "fail" || next.kind === "trust") {
            fail(next.kind === "fail" ? next.message : HOST_KEY_CHANGED, next);
            return;
          }
          state.next = next;
          setStep("enroll", { status: "done" });
        } else if (steps.enroll.status !== "done") {
          setStep("enroll", { status: "done", note: "already set up" });
        }

        current = "install";
        if (next.kind === "install") {
          setStep("install", { status: "running" });
          const installed = await installTerminalServer(null, input);
          if (!live()) return;
          if (!installed.ok) {
            fail(installed.message, null, installed.code === "interrupted");
            return;
          }
          state.next = { kind: "ready" };
          setStep("install", {
            status: "done",
            note: installed.logoutWarning ?? `v${installed.version}`,
          });
        } else if (steps.install.status !== "done") {
          setStep("install", { status: "done", note: "already installed" });
        }

        current = "save";
        setStep("save", { status: "running" });
        state.saveTried = true;
        const saved = await saveMachine(idFor(input.label), input, scope, true);
        state.savedId = saved.machineId;
        if ((saved.binding?.autoConnect === true) !== autoConnect)
          await setMachineAutoConnect(saved.machineId, autoConnect).catch((e: unknown) =>
            toast.error(
              `Couldn't turn ${autoConnect ? "on" : "off"} connecting ${saved.label} when CodeSense opens: ${getErrorMessage(e)}. Change it from its row in Machines.`,
            ),
          );
        await reloadMachines(teamKey);
        if (!live()) return;
        setStep("save", { status: "done", note: saved.label });
      }

      current = "connect";
      setStep("connect", { status: "running" });
      const connected = await connectMachineFor(state.savedId!);
      if (!live()) return;
      if (connected.status.state !== "connected") {
        fail(
          connected.status.state === "connectionFailed"
            ? connected.status.lastError
            : `${connected.label} didn't connect (${connected.status.state}).`,
        );
        return;
      }
      setStep("connect", { status: "done" });

      current = "shell";
      const viewer = useViewerStore.getState();
      const workspaceId = activeWorkspaceIdForTeam(
        teamKey,
        viewer.activeWorkspace,
        viewer.workspaces,
      );
      if (viewer.workspaces[teamKey]?.some((w) => w.id === workspaceId)) {
        openShellOn(connected, teamKey);
        setStep("shell", { status: "done", note: "in your home folder" });
        toast.success(`Added ${connected.label} — your shell is open`);
      } else {
        setStep("shell", { status: "done", note: "open a workspace, then New terminal" });
        toast.success(`Added ${connected.label}`);
      }
      closeOnce();
    } catch (e) {
      fail(getErrorMessage(e));
    }
  };

  const savePassphrase = async () => {
    setCapturing(true);
    try {
      const forKey = keyPath.trim();
      const handle = await captureMachinePassphrase(forKey);
      if (handle && keyPathRef.current.trim() === forKey) {
        setPassphraseRef(handle);
        edited();
      }
    } catch (e) {
      toast.error(`Couldn't keep the passphrase: ${getErrorMessage(e)}`);
    } finally {
      setCapturing(false);
    }
  };

  const onFix = (action: FixAction) => {
    switch (action) {
      case "editAddress":
        edited();
        requestAnimationFrame(() => addressRef.current?.focus());
        return;
      case "useJumpHost":
        edited();
        setShowMore(true);
        requestAnimationFrame(() => jumpRef.current?.focus());
        return;
      case "chooseKey":
        edited();
        setShowMore(true);
        setMethod("keyFile");
        requestAnimationFrame(() => keyRef.current?.focus());
        return;
      case "copyPublicKey":
        if (method === "agent" && agentKey) handleCopy(agentKey.publicKey);
        else toast.error("No ssh-agent key found. Add one with ssh-add, then copy it.");
        return;
      case "checkAgain":
        // The key the person trusted is no longer the machine's: forget it
        // and look again, so they see the key it presents now.
        trusted.current = null;
        fingerprintSeen.current = false;
        edited();
        void check();
        return;
      case "retry":
        if (phase.kind === "running") void trustAndConnect(null);
        else void check();
        return;
    }
  };

  /** ✕ before the machine is saved keeps a personal `Needs setup` row — never a
   *  passphrase's handle, never shared with the team untested. */
  const closeKeepingDraft = async () => {
    if (closed.current) return;
    const saved = progress.current.savedId !== null;
    // A run still going saves the machine itself; a draft beside it would be a twin.
    const inFlight =
      phase.kind === "running" && !STEP_ORDER.some((id) => steps[id].status === "failed");
    if (!saved && !inFlight && target && fingerprintSeen.current) {
      try {
        const name = label.trim() || target.alias?.alias || target.host;
        const kept = await saveMachine(
          idFor(name),
          {
            label: name,
            host: target.host,
            port: target.port,
            username: target.username,
            jumpHost: jumpHost.trim() || null,
            signIn:
              method === "agent" || keyPath.trim() === ""
                ? { method: "agent" }
                : { method: "keyFile", keyPath: keyPath.trim() },
            trustedHostKeyFp: trusted.current?.host === target.host ? trusted.current.fp : null,
          },
          { kind: "personal" },
          false,
        );
        await reloadMachines(teamKey);
        toast(`Saved ${kept.label} — finish setting it up any time`);
      } catch (e) {
        toast.error(`Couldn't save the draft: ${getErrorMessage(e)}`);
      }
    }
    closeOnce();
  };

  const running = phase.kind === "running";
  const canCopyKey = method === "agent" && agentKey !== null;
  const busy = phase.kind === "checking" || running;
  const failedStep = STEP_ORDER.find((id) => steps[id].status === "failed");
  const canContinue = draft(null) !== null && !busy;
  const where = target
    ? formatMachineAddress({ username: null, host: target.host, port: target.port })
    : "";

  return (
    <ReservePanel
      testId="add-machine-panel"
      icon={<Server size={16} aria-hidden="true" />}
      title="Add machine"
      subtitle="Type its address. CodeSense signs in, sets up your private account, installs what it needs and opens a shell."
      closeLabel="Close add-machine panel"
      onClose={() => void closeKeepingDraft()}
      footer={
        <>
          <Button variant="outline" size="sm" onClick={closeOnce}>
            Cancel
          </Button>
          {phase.kind === "confirm" ? (
            <Button
              size="sm"
              autoFocus
              data-testid="add-machine-trust-connect"
              onClick={() => void trustAndConnect(phase.fingerprint)}
            >
              Trust &amp; connect
            </Button>
          ) : running ? (
            <Button size="sm" disabled={!failedStep} onClick={() => onFix("retry")}>
              {failedStep ? "Retry" : "Connecting…"}
            </Button>
          ) : (
            <Button
              size="sm"
              type="submit"
              form="add-machine-form"
              data-testid="add-machine-continue"
              disabled={!canContinue}
            >
              {phase.kind === "checking" ? "Connecting…" : "Continue"}
            </Button>
          )}
        </>
      }
    >
      <form
        id="add-machine-form"
        className="mt-4 flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (canContinue) void check();
        }}
      >
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="machine-address">Address</Label>
          <Input
            id="machine-address"
            ref={addressRef}
            list="machine-ssh-aliases"
            placeholder="root@203.0.113.7 or an ~/.ssh/config host"
            value={address}
            disabled={running}
            onChange={(e) => {
              setAddress(e.target.value);
              takeAlias(e.target.value);
              fingerprintSeen.current = false;
              edited();
            }}
          />
          {sources && sources.configHosts.length > 0 && (
            <datalist id="machine-ssh-aliases">
              {sources.configHosts.map((h) => (
                <option key={h.alias} value={h.alias} />
              ))}
            </datalist>
          )}
          {address.trim() && !target && (
            <p className="text-[11px] text-destructive">
              Use host, user@host, user@host:port, or a host from ~/.ssh/config.
            </p>
          )}
          {target?.alias && (
            <p data-testid="machine-alias-summary" className="text-[11px] text-muted-foreground">
              {formatMachineAddress(target)}
              {target.alias.proxyJump ? ` via ${target.alias.proxyJump}` : ""}
              {proxyCommand ? ` through ${proxyCommand}` : ""}
              {target.alias.identityFile ? ` with ${target.alias.identityFile}` : ""} from
              ~/.ssh/config
              {proxyCommand && scope.kind === "team"
                ? `. Teammates connect through their own ~/.ssh/config entry for ${target.alias.alias}.`
                : ""}
            </p>
          )}
          <p data-testid="machine-signin-summary" className="text-[11px] text-muted-foreground">
            {method === "agent"
              ? agentKey
                ? `Signs in with your ssh-agent key ${agentKey.keyType} ${agentKey.fingerprint}${agentKey.comment ? ` (${agentKey.comment})` : ""}${agentKey.identityFile ? ", offering no other key" : ""}.`
                : "Signs in with your keys in ~/.ssh."
              : method === "keyFile"
                ? `Signs in with ${keyPath.trim() || "a key file"}.`
                : `Signs in with ${keyPath.trim() || "a key"} and its passphrase from your keychain.`}{" "}
            <button
              type="button"
              className="underline-offset-2 hover:underline"
              onClick={() => setShowMore(true)}
              disabled={running}
            >
              Change
            </button>
          </p>
        </div>

        {!showMore ? (
          <button
            type="button"
            data-testid="add-machine-more"
            onClick={() => setShowMore(true)}
            className="self-start text-xs text-muted-foreground hover:text-foreground"
            disabled={running}
          >
            More options
          </button>
        ) : (
          <fieldset disabled={running} className="flex flex-col gap-3">
            <div className="flex flex-col gap-1.5">
              <Label>Sign in with</Label>
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
                      edited();
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
            </div>
            {method === "agent" && agentKeys.length > 1 && (
              <div role="radiogroup" aria-label="Agent key" className="flex flex-col gap-1">
                {agentKeys.map((k) => (
                  <label key={k.fingerprint} className="flex items-center gap-2 text-xs">
                    <input
                      type="radio"
                      name="machine-agent-key"
                      data-testid={`machine-agent-key-${k.comment || k.fingerprint}`}
                      checked={agentKey?.fingerprint === k.fingerprint}
                      onChange={() => {
                        setAgentFp(k.fingerprint);
                        edited();
                      }}
                    />
                    <span>
                      {k.keyType} {k.fingerprint}
                      {k.comment ? ` (${k.comment})` : ""}
                    </span>
                  </label>
                ))}
              </div>
            )}
            {method !== "agent" && (
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="machine-key">Key file</Label>
                <Input
                  id="machine-key"
                  ref={keyRef}
                  value={keyPath}
                  onChange={(e) => {
                    setKeyPath(e.target.value);
                    setPassphraseRef(null);
                    edited();
                  }}
                />
                {!keyPathOk && (
                  <p className="text-[11px] text-destructive">
                    Use a path to the key file, like ~/.ssh/id_ed25519.
                  </p>
                )}
              </div>
            )}
            {passphraseMachine && (
              <div className="flex flex-col gap-1.5">
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    type="button"
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
                  macOS asks for it in its own dialog. {CREDENTIALS_IN_KEYCHAIN}, never shown or
                  sent anywhere else.
                </p>
              </div>
            )}
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="machine-label">Name</Label>
              <Input
                id="machine-label"
                placeholder={target?.alias?.alias ?? target?.host ?? "devbox-01"}
                value={label}
                onChange={(e) => setLabel(e.target.value)}
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="machine-jump">Jump host</Label>
              <Input
                id="machine-jump"
                ref={jumpRef}
                placeholder="bastion (optional)"
                value={jumpHost}
                onChange={(e) => {
                  setJumpHost(e.target.value);
                  edited();
                }}
              />
              {!jumpOk && (
                <p className="text-[11px] text-destructive">Use host or user@host:port.</p>
              )}
            </div>
            {team && (
              <div role="radiogroup" aria-label="Who sees this machine" className="flex gap-4">
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
              </div>
            )}
            <label className="flex items-center gap-2 text-xs">
              <input
                type="checkbox"
                data-testid="machine-auto-connect"
                checked={autoConnect}
                disabled={passphraseMachine}
                onChange={(e) => setAutoConnectChoice(e.target.checked)}
              />
              Connect when CodeSense opens
            </label>
            <p className="text-[11px] text-muted-foreground">
              {passphraseMachine
                ? "A machine whose key needs a passphrase connects when you pick it, never at launch."
                : "Machines with open tabs always reconnect when CodeSense opens. Reconnecting never asks for a password or trusts a new key."}
            </p>
            {method === "agent" && !passphraseMachine && (
              <p
                data-testid="machine-agent-confirm-hint"
                className="text-[11px] text-muted-foreground"
              >
                If your SSH agent asks you to approve each use, CodeSense never reconnects through
                1Password at launch, but it can't tell when an agent with ssh-add -c will ask: turn
                this off for such a key.
              </p>
            )}
          </fieldset>
        )}
      </form>

      {phase.kind === "checking" && (
        <p className="mt-4 flex items-center gap-1.5 text-xs text-muted-foreground">
          <Loader2 size={12} className="animate-spin" aria-hidden="true" />
          Connecting to {where}…
        </p>
      )}

      {phase.kind === "confirm" && (
        <div
          data-testid="add-machine-fingerprint"
          className="mt-4 flex flex-col gap-1.5 rounded-md border border-border p-3 text-xs"
        >
          <p>
            First time connecting to {where}. Its fingerprint is{" "}
            <code className="rounded bg-muted px-1 text-[11px]">{phase.fingerprint}</code>.
          </p>
          <p className="text-[11px] text-muted-foreground">
            If your cloud console shows the machine's SSH fingerprint, check it matches. Trust &amp;
            connect pins this key: CodeSense refuses the machine if it ever changes.
          </p>
        </div>
      )}

      {phase.kind === "formError" && (
        <FailureNotice
          testId="add-machine-error"
          message={phase.message}
          fix={phase.fix}
          onFix={onFix}
          canCopyKey={canCopyKey}
        />
      )}

      {running && (
        <ul data-testid="add-machine-progress" className="mt-4 flex flex-col gap-1.5">
          {STEP_ORDER.map((id) => (
            <ProgressRow
              key={id}
              id={id}
              state={steps[id]}
              user={target?.username ?? null}
              onFix={onFix}
              canCopyKey={canCopyKey}
            />
          ))}
        </ul>
      )}
    </ReservePanel>
  );
}

function freshSteps(): Record<StepId, StepState> {
  return Object.fromEntries(STEP_ORDER.map((id) => [id, { status: "pending" }])) as Record<
    StepId,
    StepState
  >;
}

const STEP_LABEL: Record<StepId, string> = {
  trust: "Trust host key",
  signIn: "Sign in",
  enroll: "Set up your private account",
  install: "Install terminal server",
  save: "Save machine",
  connect: "Connect",
  shell: "Open a shell",
};

function ProgressRow({
  id,
  state,
  user,
  onFix,
  canCopyKey,
}: {
  id: StepId;
  state: StepState;
  user: string | null;
  onFix: (action: FixAction) => void;
  canCopyKey: boolean;
}) {
  const glyph =
    state.status === "done" ? (
      <span className={TONE_CLASS.ok}>✓</span>
    ) : state.status === "failed" ? (
      <span className={TONE_CLASS.error}>✕</span>
    ) : state.status === "running" ? (
      <Loader2 size={12} className="animate-spin" aria-label="running" />
    ) : (
      <span className="text-muted-foreground">·</span>
    );
  const name = id === "signIn" && user ? `Sign in as ${user}` : STEP_LABEL[id];
  return (
    <li
      data-testid={`add-machine-step-${id}`}
      data-status={state.status}
      className="flex flex-col gap-1 text-xs"
    >
      <div className="flex items-start gap-2">
        <span className="flex w-3 shrink-0 justify-center" aria-hidden={state.status !== "running"}>
          {glyph}
        </span>
        <span className={cn("font-medium", state.status === "pending" && "text-muted-foreground")}>
          {name}
        </span>
        {state.status === "done" && state.note && (
          <span className="min-w-0 truncate text-muted-foreground">{state.note}</span>
        )}
      </div>
      {state.status === "failed" && (
        <div className="ml-5">
          <FailureNotice
            testId={`add-machine-step-${id}-error`}
            canCopyKey={canCopyKey}
            message={state.message}
            fix={
              state.resumable
                ? {
                    fix: "The upload broke off. Retry picks up where it stopped.",
                    actions: ["retry"],
                  }
                : state.fix
            }
            onFix={onFix}
          />
        </div>
      )}
    </li>
  );
}

const FIX_LABEL: Record<FixAction, string> = {
  editAddress: "Edit address",
  useJumpHost: "Use a jump host",
  chooseKey: "Choose a key",
  copyPublicKey: "Copy my public key",
  checkAgain: "Check again",
  retry: "Retry",
};

function FailureNotice({
  testId,
  message,
  fix,
  onFix,
  canCopyKey,
}: {
  testId: string;
  message: string;
  fix: FailureFix;
  onFix: (action: FixAction) => void;
  /** An agent key is the one in use, so there is a public key to copy. */
  canCopyKey: boolean;
}) {
  const actions = fix.actions.filter((a) => a !== "copyPublicKey" || canCopyKey);
  return (
    <div role="alert" data-testid={testId} className="mt-2 flex flex-col gap-1.5 text-xs">
      <p className="text-destructive">{message}</p>
      {fix.fix && <p className="text-muted-foreground">{fix.fix}</p>}
      {actions.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {actions.map((action) => (
            <button
              key={action}
              type="button"
              data-testid={`add-machine-fix-${action}`}
              onClick={() => onFix(action)}
              className="rounded border border-border px-1.5 py-0.5 hover:bg-accent"
            >
              {FIX_LABEL[action]}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
