import { useState } from "react";
import { WorkspaceGroupMenu } from "../../../src/features/viewer/components/workspace-group-menu";
import { TerminalTabBar } from "../../../src/features/viewer/components/terminal-tab-bar";
import { NotesList } from "../../../src/features/notes/components/notes-list";
import { InlineRename } from "../../../src/shared/components/inline-rename";
import { TooltipProvider } from "../../../src/components/ui/tooltip";
import { MetadataListChipEditor } from "../../../src/features/viewer/components/metadata-list-chip-editor";
import {
  MetadataTextField,
  MetadataStatusCombobox,
} from "../../../src/features/viewer/components/metadata-field-controls";
import { GeneralSection } from "../../../src/features/settings/components/general-section";
import { useFieldDraft } from "../../../src/features/settings/lib/use-field-draft";

function DraftField({ record }: { record: (name: string) => void }) {
  const [committed, setCommitted] = useState("Original");
  const field = useFieldDraft({
    committed,
    label: "Main doc",
    commit: async (next) => {
      record(next);
      setCommitted(next);
    },
  });
  return (
    <input
      aria-label="Main doc"
      value={field.draft}
      onChange={field.onChange}
      onBlur={field.onBlur}
      onKeyDown={field.onKeyDown}
      disabled={field.isCommitting}
    />
  );
}

export function NativeEditors({ mode, record }: { mode: string; record: (name: string) => void }) {
  const [open, setOpen] = useState(true);
  const [value, setValue] = useState<string | null>("Original");
  const commit = (next: string | null) => {
    setValue(next);
    record(next ?? "");
  };
  return (
    <TooltipProvider>
      <button id="native-outside">Outside editor</button>
      {mode === "metadata-chip" ? (
        <MetadataListChipEditor
          items={[]}
          ariaLabel="depends_on"
          onChange={(items) => record(items.join(","))}
        />
      ) : mode === "metadata-text" ? (
        <MetadataTextField ariaLabel="Metadata text" value={value} onChange={commit} />
      ) : mode === "metadata-status" ? (
        <MetadataStatusCombobox ariaLabel="Metadata status" value={value} onChange={commit} />
      ) : mode === "font" ? (
        <GeneralSection query="viewer font size" />
      ) : mode === "field-draft" ? (
        <DraftField record={record} />
      ) : mode === "group" || mode === "new-group" ? (
        open && (
          <WorkspaceGroupMenu
            teamKey="fixture:dev"
            groupId={mode === "group" ? "fixture-group" : undefined}
            workspace={
              mode === "new-group"
                ? { id: "fixture-workspace", kind: "story", cwd: "/fixture", status: "ready" }
                : undefined
            }
            position={{ x: 300, y: 20 }}
            onClose={() => setOpen(false)}
          />
        )
      ) : mode === "terminal" ? (
        <TerminalTabBar
          tabs={[{ id: "native-tab", sessionId: "native-session", name: "Original" }]}
          activeTabId="native-tab"
          renameRequest={{ tabId: "native-tab", nonce: 1 }}
          onSelectTab={() => {}}
          onCloseTab={() => {}}
          onNewShell={() => {}}
          onReorderTab={() => {}}
          onRenameTab={(_, name) => record(name)}
        />
      ) : mode === "notes" ? (
        <NotesList
          notes={[{ id: "native-note", title: "Original", pinned: false, lastOpenedAt: 0 }]}
          activeNoteId="native-note"
          summaries={new Map()}
          now={0}
          variant="column"
          onSelect={() => {}}
          onCreate={() => {}}
          onRename={(_, name) => record(name)}
          onRequestDelete={() => {}}
          onTogglePin={() => {}}
        />
      ) : (
        open && (
          <InlineRename
            value="Original"
            ariaLabel="Inline name"
            onSubmit={(name) => {
              record(name);
              setOpen(false);
            }}
            onCancel={() => setOpen(false)}
          />
        )
      )}
    </TooltipProvider>
  );
}
