// SPDX-License-Identifier: MIT
import { useEffect, useId, useRef, useState } from "react";
import { homeDir } from "@tauri-apps/api/path";
import { FolderOpen, Plus } from "lucide-react";

import { AppButton } from "@/shared/components/app-button";
import { MENU_ITEM, MenuLeadIcon } from "@/shared/components/menu-section";
import { Input } from "@/components/ui/input";
import {
  NOT_A_CONTEXT_REPO,
  contextRepoFolderName,
  defaultContextRepoLocation,
  folderName,
} from "@/shared/lib/codesense-docs";

import {
  chooseFolder,
  connectContextRepo,
  createContextRepo,
  type ContextRepoOutcome,
} from "../lib/context-repo-actions";
import { useLocalSetupActivity } from "../lib/local-setup-completion";

/**
 * Where the add-a-context-repo flow is (SW-OC-16). Owned by the host so it survives the
 * host closing and reopening (the switcher popover unmounts its content).
 */
export type ContextRepoStep =
  | { kind: "idle" }
  | { kind: "confirm"; path: string }
  | { kind: "not-a-context-repo"; path: string }
  | { kind: "error"; message: string }
  | { kind: "create" };

export const IDLE_STEP: ContextRepoStep = { kind: "idle" };

interface FlowProps {
  step: ContextRepoStep;
  setStep: (step: ContextRepoStep) => void;
  /** A context repo was connected or created and is now active. */
  onDone?: () => void;
  /** The OS folder picker returned (hosts that may have closed reopen here). */
  onPickerSettled?: () => void;
  /** `menu`: the switcher footer's rows; `buttons`: a full-width page layout. */
  variant?: "menu" | "buttons";
}

const MENU_ROW = MENU_ITEM;

/** Connect (primary) and Create (secondary), then the inline confirm / create form. */
export function ContextRepoFlow({
  step,
  setStep,
  onDone,
  onPickerSettled,
  variant = "menu",
}: FlowProps) {
  const busy = useLocalSetupActivity((s) => s.busy);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const settle = (outcome: ContextRepoOutcome, path: string) => {
    if (!outcome || !live.current) return;
    if (outcome.ok) {
      setStep(IDLE_STEP);
      onDone?.();
    } else if (outcome.failure.kind === "not-a-context-repo") {
      setStep({ kind: "not-a-context-repo", path });
    } else {
      setStep({ kind: "error", message: outcome.failure.message });
    }
  };

  async function pick() {
    let path: string | null = null;
    try {
      path = await chooseFolder();
    } catch (error) {
      setStep({ kind: "error", message: String(error) });
    }
    onPickerSettled?.();
    if (path && live.current) setStep({ kind: "confirm", path });
  }

  async function connect(path: string) {
    settle(await connectContextRepo(path), path);
  }

  if (step.kind === "create") {
    return (
      <CreateContextRepoForm
        onCancel={() => setStep(IDLE_STEP)}
        onCreated={() => {
          setStep(IDLE_STEP);
          onDone?.();
        }}
        onPickerSettled={onPickerSettled}
      />
    );
  }

  if (step.kind === "confirm") {
    const name = folderName(step.path);
    return (
      <div className="space-y-2 p-2" data-testid="context-repo-connect-confirm">
        <p className="text-sm font-medium">Connect {name}?</p>
        <p className="text-xs text-muted-foreground">
          CodeSense will read and write files in{" "}
          <span className="font-mono break-all text-foreground">{step.path}</span>. It won't run git
          — you or your agents do.
        </p>
        <div className="flex flex-wrap gap-2 pt-1">
          <AppButton
            size="sm"
            disabled={busy}
            data-testid="context-repo-connect-submit"
            onClick={() => void connect(step.path)}
          >
            {busy ? "Connecting…" : "Connect"}
          </AppButton>
          <AppButton size="sm" variant="outline" disabled={busy} onClick={() => void pick()}>
            Choose another folder
          </AppButton>
        </div>
      </div>
    );
  }

  if (step.kind === "not-a-context-repo" || step.kind === "error") {
    return (
      <div className="space-y-2 p-2" data-testid={`context-repo-${step.kind}`}>
        <p role="alert" className="text-xs text-destructive">
          {step.kind === "error" ? step.message : NOT_A_CONTEXT_REPO}
        </p>
        <div className="flex flex-wrap gap-2 pt-1">
          <AppButton size="sm" variant="outline" disabled={busy} onClick={() => void pick()}>
            Choose another folder
          </AppButton>
          <AppButton
            size="sm"
            variant="ghost"
            disabled={busy}
            onClick={() => setStep({ kind: "create" })}
          >
            Create a new context repo…
          </AppButton>
        </div>
      </div>
    );
  }

  if (variant === "buttons") {
    return (
      <div className="flex flex-wrap gap-2">
        <AppButton disabled={busy} data-testid="context-repo-connect" onClick={() => void pick()}>
          <FolderOpen aria-hidden="true" />
          Connect a context repo…
        </AppButton>
        <AppButton
          variant="outline"
          disabled={busy}
          data-testid="context-repo-create"
          onClick={() => setStep({ kind: "create" })}
        >
          <Plus aria-hidden="true" />
          Create a new context repo…
        </AppButton>
      </div>
    );
  }

  return (
    <div className="flex flex-col">
      <button
        type="button"
        role="menuitem"
        className={MENU_ROW}
        disabled={busy}
        data-testid="context-repo-connect"
        onClick={() => void pick()}
      >
        <MenuLeadIcon>
          <FolderOpen size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Connect a context repo…</span>
      </button>
      <button
        type="button"
        role="menuitem"
        className={MENU_ROW}
        disabled={busy}
        data-testid="context-repo-create"
        onClick={() => setStep({ kind: "create" })}
      >
        <MenuLeadIcon>
          <Plus size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Create a new context repo…</span>
      </button>
    </div>
  );
}

/** Name + Location (default `~/CodeSense/<name>`) + "Start from the CodeSense example". */
export function CreateContextRepoForm({
  onCreated,
  onCancel,
  onPickerSettled,
}: {
  onCreated: () => void;
  onCancel: () => void;
  onPickerSettled?: () => void;
}) {
  const busy = useLocalSetupActivity((s) => s.busy);
  const ids = useId();
  const [name, setName] = useState("");
  const [home, setHome] = useState<string | null>(null);
  // Once the user picks or types a location, it stops following the name.
  const [location, setLocation] = useState<string | null>(null);
  const [fromExample, setFromExample] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    homeDir().then(
      (dir) => live.current && setHome(dir),
      () => {},
    );
    return () => {
      live.current = false;
    };
  }, []);

  const effectiveLocation =
    location ?? (home ? defaultContextRepoLocation(home, name || "my-context") : "");

  async function chooseLocation() {
    try {
      const parent = await chooseFolder("Choose where to create the context repo");
      onPickerSettled?.();
      if (parent && live.current) {
        setLocation(`${parent.replace(/[/\\]+$/, "")}/${contextRepoFolderName(name)}`);
      }
    } catch (e) {
      if (live.current) setError(String(e));
    }
  }

  async function submit() {
    setError(null);
    const outcome = await createContextRepo(name.trim(), effectiveLocation.trim(), fromExample);
    if (!outcome || !live.current) return;
    if (outcome.ok) onCreated();
    else setError(outcome.failure.message);
  }

  return (
    <form
      className="space-y-3 p-2"
      aria-label="Create a new context repo"
      data-testid="context-repo-create-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <p className="text-sm font-medium">Create a new context repo</p>
      <label className="block space-y-1 text-xs" htmlFor={`${ids}-name`}>
        <span className="text-muted-foreground">Name</span>
        <Input
          id={`${ids}-name`}
          data-testid="context-repo-create-name"
          value={name}
          disabled={busy}
          required
          autoFocus
          placeholder="Acme"
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      <div className="space-y-1 text-xs">
        <label className="text-muted-foreground" htmlFor={`${ids}-location`}>
          Location
        </label>
        <div className="flex gap-1.5">
          <Input
            id={`${ids}-location`}
            data-testid="context-repo-create-location"
            className="font-mono text-xs"
            value={effectiveLocation}
            disabled={busy}
            required
            onChange={(e) => setLocation(e.target.value)}
          />
          <AppButton
            type="button"
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={() => void chooseLocation()}
          >
            Choose…
          </AppButton>
        </div>
      </div>
      <label className="flex items-start gap-2 text-xs">
        <input
          type="checkbox"
          className="mt-0.5 accent-primary"
          data-testid="context-repo-create-example"
          checked={fromExample}
          disabled={busy}
          onChange={(e) => setFromExample(e.target.checked)}
        />
        <span>
          <span className="font-medium text-foreground">Start from the CodeSense example</span>
          <span className="block text-muted-foreground">
            An editable copy of the CodeSense docs setup: a team, skills, a dashboard and a routine.
          </span>
        </span>
      </label>
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <AppButton
          type="submit"
          size="sm"
          data-testid="context-repo-create-submit"
          disabled={busy || !name.trim() || !effectiveLocation.trim()}
        >
          {busy ? "Creating…" : "Create and connect"}
        </AppButton>
        <AppButton type="button" size="sm" variant="outline" disabled={busy} onClick={onCancel}>
          Cancel
        </AppButton>
      </div>
    </form>
  );
}
