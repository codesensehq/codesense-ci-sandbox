// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useEffect, useState } from "react";
import { TooltipProvider } from "@/components/ui/tooltip";
import type { AgentDefinition, AgentLaunchAdmission } from "@/tauri/commands";
import golden from "../../../../src-tauri/src/commands/agents_launch/adapter_launch_golden.json";
import { draftFromDefinition, type AgentDraft } from "../lib/agent-form";
import { splitArgs } from "@/shared/lib/tokenize-args";
import { AgentRunsOnFields } from "./agent-form-fields";

type NativeRow = { definition: AgentDefinition; launch_admission: AgentLaunchAdmission };
function row(name: string): NativeRow {
  return (golden as unknown as Record<string, NativeRow>)[`${name} Interactive`];
}
let latest: AgentDraft;
function Harness({ initial, admission }: { initial: NativeRow; admission: AgentLaunchAdmission }) {
  const [draft, setDraft] = useState(() => draftFromDefinition(initial.definition));
  useEffect(() => {
    latest = draft;
  }, [draft]);
  return (
    <TooltipProvider>
      <AgentRunsOnFields
        draft={draft}
        onChange={setDraft}
        admission={admission}
        escalating={false}
      />
    </TooltipProvider>
  );
}
afterEach(cleanup);

describe("Hermes endpoint editor consumes native model facts", () => {
  it.each([
    "hermes-native-model-alias",
    "hermes-native-model-joined",
    "hermes-native-model-grouped",
  ])(
    "%s replaces every model alias using the backend edit slot and keeps typing responsive",
    (name) => {
      const native = row(name);
      render(<Harness initial={native} admission={native.launch_admission} />);
      const model = screen.getByLabelText("Model");
      expect(model).toHaveValue("second");
      fireEvent.change(model, { target: { value: "third" } });
      expect(model).not.toBeDisabled();
      fireEvent.change(model, { target: { value: "fourth" } });
      const expected = [...native.launch_admission.model_arguments!.edit!.args];
      expected[native.launch_admission.model_arguments!.edit!.value_index] = "fourth";
      expect(splitArgs(latest.args)).toEqual(expected);
      expect(model).toHaveValue("fourth");
    },
  );

  it("keeps --mo=new Extra args verbatim and waits for a matching rewrite fact", () => {
    const initial = row("hermes-native-model-alias");
    const fresh = row("hermes-native-model-extra");
    const view = render(<Harness initial={initial} admission={initial.launch_admission} />);
    fireEvent.change(screen.getByLabelText("Args"), { target: { value: "--mo=new" } });
    expect(latest.args).toBe("--mo=new");
    expect(screen.getByLabelText("Model")).toBeDisabled();
    view.rerender(<Harness initial={initial} admission={fresh.launch_admission} />);
    expect(screen.getByLabelText("Model")).toHaveValue("new");
    expect(screen.getByLabelText("Model")).not.toBeDisabled();
    expect(latest.args).toBe("--mo=new");
  });

  it("updates the configured model without appending argv when no model flag exists", () => {
    const native = row("hermes-open");
    render(<Harness initial={native} admission={native.launch_admission} />);
    const before = latest.args;
    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "new-local" } });
    expect(latest.args).toBe(before);
    expect((latest.startupSettings?.model as { default: string }).default).toBe("new-local");
    expect(screen.getByLabelText("Model")).toHaveValue("new-local");
  });
});
