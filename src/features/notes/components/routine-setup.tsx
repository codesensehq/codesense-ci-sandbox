// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";

import { openAgents } from "@/features/agents/agents-virtual-tab";
import { useAgentRows } from "@/features/agents/lib/agent-names";
import { openRoutineDraft } from "@/features/routines/routines-virtual-tab";

import { noteContextLine, readNotePrompt, type NoteRef } from "../lib/note-prompts";
import { AgentRowFace } from "./agent-row-face";
import { NtPopover, NtPopoverContent, NtPopoverTrigger } from "./nt-popover";
import { PaneSheet } from "./pane-sheet";

/** A field's value as a quiet button; its choices open beneath it. */
function Pick<T extends string>({
  label,
  value,
  choices,
  onChange,
  autoFocus,
}: {
  label: string;
  value: ReactNode;
  choices: readonly { id: T; label: ReactNode }[];
  onChange: (id: T) => void;
  autoFocus?: boolean;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  return (
    <NtPopover open={open} onOpenChange={setOpen}>
      <NtPopoverTrigger asChild>
        <button type="button" className="nt-pick" aria-label={label} data-autofocus={autoFocus}>
          {value}
          <ChevronDown size={12} className="nt-btn-chev" aria-hidden />
        </button>
      </NtPopoverTrigger>
      <NtPopoverContent align="start" style={{ minWidth: 200 }} aria-label={label}>
        {choices.map((c) => (
          <button
            key={c.id}
            type="button"
            className="nt-row"
            data-nt-row
            onClick={() => {
              onChange(c.id);
              setOpen(false);
            }}
          >
            {c.label}
          </button>
        ))}
      </NtPopoverContent>
    </NtPopover>
  );
}

/** When the routine runs, as the routine editor's "Every …" slot reads it. */
export const ROUTINE_WHEN_CHOICES = [
  { when: "weekday 18:00", label: "weekday at 18:00" },
  { when: "day 18:00", label: "day at 18:00" },
  { when: "day 09:00", label: "morning at 09:00" },
  { when: "mon 09:00", label: "Monday at 09:00" },
] as const;

/**
 * "Set up a routine": the few things a notes routine needs (when, which agent, what
 * to do), then the routine editor itself, filled in, to save it. The task starts from
 * the person's `prompts/tidy-notes.md`; a routine made here is theirs only.
 */
export function RoutineSetup({
  top,
  noteTitle,
  note,
  onClose,
}: {
  top: number;
  noteTitle: string;
  note: NoteRef;
  onClose: () => void;
}): React.JSX.Element {
  const { rows } = useAgentRows();
  const agents = useMemo(() => (rows ?? []).filter((a) => !a.archived), [rows]);
  const [when, setWhen] = useState<string>(ROUTINE_WHEN_CHOICES[0].when);
  const [agentKey, setAgentKey] = useState("");
  const [task, setTask] = useState<string | null>(null);
  const chosenAgent = agentKey || agents.find((a) => a.scope === "personal")?.key || agents[0]?.key;
  const chosen = agents.find((a) => a.key === chosenAgent);

  useEffect(() => {
    let cancelled = false;
    void readNotePrompt("routine").then((prompt) => {
      if (cancelled) return;
      setTask((current) => {
        if (current !== null) return current;
        const context = noteContextLine(noteTitle, note);
        return prompt ? `${prompt}\n\n${context}` : context;
      });
    });
    return () => {
      cancelled = true;
    };
  }, [noteTitle, note]);

  const create = () => {
    if (!chosenAgent) return;
    openRoutineDraft({
      draft: {
        name: "Tidy my notes",
        when,
        agent: chosenAgent,
        task: (task ?? "").trim(),
      },
      audience: "personal",
    });
    onClose();
  };

  return (
    <PaneSheet label="Set up a routine" top={top} onClose={onClose} className="nt-form">
      <div className="nt-form-title">Set up a routine</div>
      <div className="nt-form-sub">
        Saved under the agent in Agents · Schedule. Only you see it.
      </div>
      <div className="nt-field">
        <span className="nt-field-label">Every</span>
        <Pick
          label="When it runs"
          autoFocus
          value={ROUTINE_WHEN_CHOICES.find((c) => c.when === when)?.label ?? when}
          choices={ROUTINE_WHEN_CHOICES.map((c) => ({ id: c.when, label: c.label }))}
          onChange={(id) => setWhen(id)}
        />
      </div>
      <div className="nt-field">
        <span className="nt-field-label">Agent</span>
        {agents.length > 0 && chosen ? (
          <Pick
            label="Which agent"
            value={
              <>
                <AgentRowFace agent={chosen} />
                {chosen.name}
              </>
            }
            choices={agents.map((a) => ({
              id: a.key,
              label: (
                <>
                  <AgentRowFace agent={a} />
                  <span className="nt-row-label">{a.name}</span>
                </>
              ),
            }))}
            onChange={(id) => setAgentKey(id)}
          />
        ) : (
          <span className="nt-form-sub" style={{ margin: 0 }}>
            {rows === null ? "Loading agents…" : "A routine runs one of your agents. "}
            {rows !== null && (
              <button type="button" className="nt-link" onClick={() => openAgents()}>
                Make an agent
              </button>
            )}
          </span>
        )}
      </div>
      <label className="nt-field nt-field-col">
        <span className="nt-field-label">Task</span>
        <textarea
          className="nt-textarea"
          value={task ?? ""}
          placeholder="What should the agent do with your notes?"
          onChange={(e) => setTask(e.target.value)}
        />
      </label>
      <div className="nt-form-actions">
        <button type="button" className="nt-btn" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="nt-btn nt-btn-prim"
          disabled={!chosenAgent}
          onClick={create}
        >
          Create routine…
        </button>
      </div>
    </PaneSheet>
  );
}
