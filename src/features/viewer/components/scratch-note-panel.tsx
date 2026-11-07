// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useEditor, EditorContent, ReactNodeViewRenderer, type NodeViewProps } from "@tiptap/react";
import { all, createLowlight } from "lowlight";
import type { LanguageFn } from "highlight.js";
import { Lock, Search, Send, TableOfContents } from "lucide-react";
import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { agentRefNodeView } from "@/features/notes/components/agent-ref-chip";
import { ChangeBanner, HistorySheet } from "@/features/notes/components/note-changes";
import { AgentMenu, NoteActionsMenu, NotesChrome } from "@/features/notes/components/notes-chrome";
import { NotesEmpty } from "@/features/notes/components/notes-empty";
import { NotesList } from "@/features/notes/components/notes-list";
import {
  NtPopover,
  NtPopoverContent,
  NtPopoverTrigger,
} from "@/features/notes/components/nt-popover";
import { RoutineSetup } from "@/features/notes/components/routine-setup";
import { SendPicker, type AnchorRect } from "@/features/notes/components/send-picker";
import { SlashMenu } from "@/features/notes/components/slash-menu";
import { openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import { NoteAgentsProvider } from "@/features/notes/lib/note-agents";
import {
  appendAgentRef,
  blockAtPos,
  relocateBlock,
  sendTargetOf,
  type NoteBlock,
} from "@/features/notes/lib/note-blocks";
import { noteDecorationsKey } from "@/features/notes/lib/note-decorations";
import { diffAgainst } from "@/features/notes/lib/note-diff";
import { noteContextLine, readNotePrompt, type NoteRef } from "@/features/notes/lib/note-prompts";
import { sendToAgent as deliverToAgent } from "@/features/notes/lib/send-to-agent";
import { type SlashMenuController, type SlashMenuState } from "@/features/notes/lib/slash-commands";
import { useNoteHistory } from "@/features/notes/lib/use-note-history";
import { useNoteListSummaries } from "@/features/notes/lib/use-note-summaries";
import { useNow } from "@/shared/hooks/use-now";
import { formatAgo } from "@/shared/lib/format-updated-ago";
import { currentNotesScope } from "@/shared/lib/notes-scope";
import { sendTextToTerminal } from "@/shared/lib/send-to-terminal";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { useContentSource } from "@/shared/hooks/use-content-source";
import { useTeamPipelineRepoPath } from "@/shared/hooks/use-team-pipeline-repo-path";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePipelineTree } from "@/shared/lib/pipeline-graph/use-pipeline-tree";
import { derivedTreeToReactFlow } from "@/shared/lib/pipeline-graph/derived-tree-to-react-flow";
import { notesDirPath, type NoteRevision } from "@/tauri/commands";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { AppButton } from "@/shared/components/app-button";

import { CodeBlockNodeView } from "./code-block-node-view";
import { ScratchSelectionMenu, type SelectionAnchorRect } from "./scratch-selection-menu";
import { computeSelectionMenu, readActiveMarks, type ActiveMarks } from "../lib/scratch-selection";
import { handleNoteLinkHref } from "../lib/cs-link";
import {
  buildPickerItems,
  filterPickerItems,
  type DocNodeLike,
} from "../lib/note-link-picker-items";
import {
  type NoteLinkPickerState,
  type NoteLinkSuggestionController,
} from "../lib/note-link-suggestion";
import { NoteLinkPicker } from "./note-link-picker";
import {
  noteEditorExtensions,
  noteEditorProps,
  serializeEditorMarkdown,
  loadMarkdown,
} from "@/features/notes/editor/note-editor-extensions";
import { useScrollMemory } from "../lib/use-scroll-memory";
import { NoteOutline } from "./note-outline";
import { StickyNoteHeaders } from "./sticky-note-headers";
import { useNoteHeadings } from "./use-note-headings";
import { scrollToHeadingBySlug } from "../lib/note-headings";

const lowlight = createLowlight(all);

// Register a "plaintext" language that produces no highlights.
lowlight.register("plaintext", (() => ({ contains: [] })) as LanguageFn);

/** The editor's root, and the lines in it a hover send button is offered for. */
const EDITOR_ROOT = ".scratch-note-editor";
const SENDABLE_LINES = ["li", "> p", "> h1", "> h2", "> h3"]
  .map((line) => `${EDITOR_ROOT} ${line}`)
  .join(", ");

/** The titles a note has before anyone names it. */
const BLANK_TITLES = new Set(["", "Note", "Untitled"]);

/** At or above this pane width the note list and the outline become columns. */
export const NOTES_WIDE_PX = 880;

/** Ref read by code-block node views at click time: opens "Send to…" for a block. */
const _codeSendRef = { fn: null as ((text: string, anchor: HTMLElement) => void) | null };

function CodeBlockWithSend(props: NodeViewProps) {
  return <CodeBlockNodeView {...props} onSend={_codeSendRef.fn ?? undefined} />;
}

const EMPTY_MARKS: ActiveMarks = { bold: false, italic: false, code: false };

/** Read the current DOM selection's viewport rect, or null if there is no live
 *  on-screen range (e.g. a programmatic PM selection with no DOM range). */
function readSelectionRect(): SelectionAnchorRect | null {
  const domSel = window.getSelection();
  if (!domSel || domSel.rangeCount === 0) return null;
  const r = domSel.getRangeAt(0).getBoundingClientRect();
  if (r.width === 0 && r.height === 0) return null;
  return { top: r.top, left: r.left, width: r.width, height: r.height };
}

function rectOf(el: Element): AnchorRect {
  const r = el.getBoundingClientRect();
  return { top: r.top, left: r.left, width: r.width, height: r.height };
}

/** What "Send to…" was opened for. */
interface SendRequest {
  seq: number;
  text: string;
  /** The whole line being handed off (so it can carry a chip), if it is one. */
  block: NoteBlock | null;
  anchor: AnchorRect;
  /** The text already names the note (the whole note is being handed over). */
  namesNote?: boolean;
}

/** The line a hovered send button belongs to, in sheet coordinates. */
interface HoverLine {
  pos: number;
  top: number;
  left: number;
}

const INCLUDE_CONTEXT_KEY = "cs.notes.send.include-context";

function readIncludeContext(): boolean {
  try {
    return localStorage.getItem(INCLUDE_CONTEXT_KEY) !== "false";
  } catch {
    return true;
  }
}

/**
 * The Notes pane: one quiet sheet. The note's title and meta live at the top of the
 * sheet; one chrome row holds the list, "Find a note", the outline and the Agent menu.
 * The list and outline are a drawer and a popover when the pane is narrow, columns
 * when it is wide. Any line can be handed to an agent; the line then carries a chip
 * with that agent's live status. No props — the right-pane stack owns the title bar.
 */
export function ScratchNotePanel() {
  const { teamKey } = useActiveWorkspaceContext();
  return (
    <NoteAgentsProvider teamKey={teamKey}>
      <NotesPane />
    </NoteAgentsProvider>
  );
}

function NotesPane() {
  const content = useViewerStore((s) => s.scratchNoteContent);
  const setScratchNoteContent = useViewerStore((s) => s.setScratchNoteContent);
  const loadNotes = useViewerStore((s) => s.loadNotes);
  // SLW-PT-7: notes resolve to the caller's personal team, keyed by github id. The
  // reactive hook lets the load effect below re-fire when auth resolves (id 0→real
  // at boot) or the account switches.
  const githubUserId = useCurrentUser()?.github_user_id ?? 0;
  // SLW-PT-19: notes are per ORG — switching org switches the notes shown.
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const orgName = useOrgStore(
    (s) =>
      s.orgs.find((o) => o.slug === s.activeOrgSlug)?.display_name ?? s.activeOrgSlug ?? "this Mac",
  );
  const flushNotes = useViewerStore((s) => s.flushNotes);
  const scratchNoteLoadStatus = useViewerStore((s) => s.scratchNoteLoadStatus);
  const notes = useViewerStore((s) => s.notes);
  const activeNoteId = useViewerStore((s) => s.activeNoteId);
  const setActiveNote = useViewerStore((s) => s.setActiveNote);
  const createNote = useViewerStore((s) => s.createNote);
  const renameNote = useViewerStore((s) => s.renameNote);
  const deleteNote = useViewerStore((s) => s.deleteNote);
  const setNotePinned = useViewerStore((s) => s.setNotePinned);
  const noteSavedAt = useViewerStore((s) => s.noteSavedAt);
  // SW-SN-6: publish the live headings (read-model for the go-to-heading modal,
  // which mounts without the editor) + consume the modal's jump signal here,
  // where the editor lives.
  const setActiveNoteHeadings = useViewerStore((s) => s.setActiveNoteHeadings);
  const pendingHeadingJump = useViewerStore((s) => s.pendingHeadingJump);
  const clearHeadingJump = useViewerStore((s) => s.clearHeadingJump);
  const setNoteSwitcherOpen = useUiStore((s) => s.setNoteSwitcherOpen);
  const setHeadingJumpOpen = useUiStore((s) => s.setHeadingJumpOpen);
  const loadedForScope = useRef<string | null>(null);

  // SW-SN-9: resolve the active workspace at action time. The warm pipeline tree
  // gives the storyId→file mapping for `cs:story` links.
  const { teamKey } = useActiveWorkspaceContext();
  const repoPath = useTeamPipelineRepoPath(null, teamKey);
  const resolvedRepo = useContentSource(repoPath);
  const teamContentPath = resolvedRepo?.teamContentPath ?? "";
  const { tree: pipelineTree, loading: pipelineLoading } = usePipelineTree(repoPath ?? "", "");
  const docNodes = useMemo<DocNodeLike[]>(
    () =>
      pipelineTree
        ? derivedTreeToReactFlow(pipelineTree).nodes.map((n) => {
            const d = n.data as DocNodeLike;
            return {
              title: d.title,
              storyId: d.storyId,
              storyFile: d.storyFile,
              resolvedSupportingDocs: d.resolvedSupportingDocs,
            };
          })
        : [],
    [pipelineTree],
  );

  // ---- pane width: narrow (drawer + popover) or wide (columns) ----
  const rootRef = useRef<HTMLDivElement>(null);
  const [wide, setWide] = useState(false);
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(([entry]) => {
      // A hidden pane (display:none) measures 0 — keep the last layout.
      const w = entry.contentRect.width;
      if (w > 0) setWide(w >= NOTES_WIDE_PX);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const [listOpen, setListOpen] = useState(false);
  const [outlineOpen, setOutlineOpen] = useState(false);
  const [sheet, setSheet] = useState<"routine" | "history" | null>(null);
  const [send, setSend] = useState<SendRequest | null>(null);
  const sendSeq = useRef(0);
  const [includeContext, setIncludeContext] = useState(readIncludeContext);
  const [hoverLine, setHoverLine] = useState<HoverLine | null>(null);
  const [emptyDismissed, setEmptyDismissed] = useState(false);

  // Scroll memory across right-pane minimize/restore: the pane hides panels with
  // `display:none`, which drops the sheet's scrollTop.
  const { attach: attachSheetScroll, resetToTop: resetSheetScroll } = useScrollMemory();
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const setScrollNode = useCallback(
    (node: HTMLDivElement | null) => {
      scrollRef.current = node;
      attachSheetScroll(node);
    },
    [attachSheetScroll],
  );

  // Ref for the ⌘↵ handler (the once-created editor keymap reads it at key time).
  const sendKeyRef = useRef<(paste: boolean) => boolean>(() => false);
  const linkClickRef = useRef<(href: string) => boolean>(() => false);
  const lastSetContent = useRef("");

  // SW-SN-9: the `[[` picker, bridged through a stable controller.
  const [picker, setPicker] = useState<NoteLinkPickerState | null>(null);
  const pickerKeyHandlerRef = useRef<((event: KeyboardEvent) => boolean) | null>(null);
  const pickerItems = useMemo(
    () => buildPickerItems(notes, docNodes, teamContentPath, activeNoteId),
    [notes, docNodes, teamContentPath, activeNoteId],
  );
  const pickerItemsRef = useRef(pickerItems);
  useEffect(() => {
    pickerItemsRef.current = pickerItems;
  }, [pickerItems]);
  const pickerGetItems = useCallback(
    (query: string) => filterPickerItems(pickerItemsRef.current, query),
    [],
  );
  const pickerOnKeyDown = useCallback(
    (event: KeyboardEvent) => pickerKeyHandlerRef.current?.(event) ?? false,
    [],
  );
  const pickerController = useMemo<NoteLinkSuggestionController>(
    () => ({ getItems: pickerGetItems, onChange: setPicker, onKeyDown: pickerOnKeyDown }),
    [pickerGetItems, pickerOnKeyDown],
  );
  const registerPickerKeyHandler = useCallback((fn: ((e: KeyboardEvent) => boolean) | null) => {
    pickerKeyHandlerRef.current = fn;
  }, []);

  // The `/` menu, bridged the same way.
  const [slash, setSlash] = useState<SlashMenuState | null>(null);
  const slashKeyHandlerRef = useRef<((event: KeyboardEvent) => boolean) | null>(null);
  const slashOnKeyDown = useCallback(
    (event: KeyboardEvent) => slashKeyHandlerRef.current?.(event) ?? false,
    [],
  );
  const slashController = useMemo<SlashMenuController>(
    () => ({ onChange: setSlash, onKeyDown: slashOnKeyDown }),
    [slashOnKeyDown],
  );
  const registerSlashKeyHandler = useCallback((fn: ((e: KeyboardEvent) => boolean) | null) => {
    slashKeyHandlerRef.current = fn;
  }, []);

  const editor = useEditor({
    extensions: noteEditorExtensions({
      lowlight,
      nodeViews: {
        agentRef: agentRefNodeView,
        codeBlock: ReactNodeViewRenderer(CodeBlockWithSend),
      },
      suggestions: { link: pickerController, slash: slashController },
      keys: { send: (paste) => sendKeyRef.current(paste) },
    }),
    content: "",
    // SW-SN-12 AC-3: start non-editable; the load-status effect below flips it on
    // once the notes model resolves.
    editable: false,
    onUpdate: ({ editor }) => {
      const preserved = serializeEditorMarkdown(editor);
      lastSetContent.current = preserved;
      setScratchNoteContent(preserved);
    },
    editorProps: noteEditorProps({ onLinkClick: (href) => linkClickRef.current(href) }),
  });

  // F-6 AC-13 / SLW-PT-7 / SLW-PT-19: load (once per profile + org).
  useEffect(() => {
    const scopeKey = `${githubUserId}@${activeOrgSlug ?? ""}`;
    if (loadedForScope.current === scopeKey) return;
    loadedForScope.current = scopeKey;
    loadNotes();
  }, [githubUserId, activeOrgSlug, loadNotes]);

  // Sync store content into the editor (on load, not on every keystroke).
  // R5C-P5: `lastSetContent` is the single source of truth for what is on screen.
  useEffect(() => {
    if (editor && content !== lastSetContent.current) {
      lastSetContent.current = content;
      // Blank-line preservation on the way in (counterpart to serializeWithBlankLines);
      // emitUpdate:false so a programmatic sync never re-triggers the save debounce.
      loadMarkdown(editor, content);
    }
  }, [editor, content]);

  // SW-SN-12/13: editable only once the model resolved AND an active note exists;
  // a stalled load still frees the pad after a grace period.
  useEffect(() => {
    if (!editor) return;
    if (scratchNoteLoadStatus !== "pending" && activeNoteId != null) {
      editor.setEditable(true);
      return;
    }
    editor.setEditable(false);
    const t = setTimeout(() => editor.setEditable(true), 3000);
    return () => clearTimeout(t);
  }, [editor, scratchNoteLoadStatus, activeNoteId]);

  // SW-SN-12 AC-4: commit the editor's latest content the instant focus leaves.
  useEffect(() => {
    if (!editor) return;
    const commitOnBlur = () => {
      const preserved = serializeEditorMarkdown(editor);
      if (preserved !== lastSetContent.current) {
        lastSetContent.current = preserved;
        setScratchNoteContent(preserved);
      }
    };
    editor.on("blur", commitOnBlur);
    return () => {
      editor.off("blur", commitOnBlur);
    };
  }, [editor, setScratchNoteContent]);

  // SW-SN-12 AC-2: flush the pending save at teardown and on window blur.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (!focused) void flushNotes();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    return () => {
      cancelled = true;
      unlisten?.();
      void flushNotes();
    };
  }, [flushNotes]);

  // A different note starts at the top.
  useEffect(() => {
    resetSheetScroll();
  }, [activeNoteId, resetSheetScroll]);

  const activeNote = notes.find((n) => n.id === activeNoteId) ?? null;

  // Where the active note's file lives (handed to agents as context).
  const [notesDir, setNotesDir] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    notesDirPath(useViewerStore.getState().notesLoadedScope ?? currentNotesScope()).then(
      (dir) => !cancelled && setNotesDir(dir),
      () => !cancelled && setNotesDir(null),
    );
    return () => {
      cancelled = true;
    };
  }, [githubUserId, activeOrgSlug]);
  const notesOrg = useViewerStore((s) => s.notesLoadedScope?.orgSlug ?? null);
  const noteRef = useMemo<NoteRef>(
    () => ({
      id: activeNoteId,
      org: notesOrg,
      path: notesDir && activeNoteId ? `${notesDir}/${activeNoteId}.md` : null,
    }),
    [activeNoteId, notesOrg, notesDir],
  );
  const noteTitle = activeNote?.title ?? "Untitled";

  // ---- "Send to…" ----
  const openSend = useCallback(
    (text: string, block: NoteBlock | null, anchor: AnchorRect, namesNote = false) => {
      sendSeq.current += 1;
      setHoverLine(null);
      setSend({ seq: sendSeq.current, text, block, anchor, namesNote });
    },
    [],
  );

  const caretAnchor = useCallback((): AnchorRect | null => {
    if (!editor) return null;
    try {
      const c = editor.view.coordsAtPos(editor.state.selection.from);
      return { top: c.top, left: c.left, width: 0, height: c.bottom - c.top };
    } catch {
      // No layout for the caret (e.g. a detached view): anchor on the editor.
      return rectOf(editor.view.dom);
    }
  }, [editor]);

  useEffect(() => {
    sendKeyRef.current = (paste) => {
      if (!editor) return false;
      const target = sendTargetOf(editor);
      if (!target) {
        toast("Put the caret on a line, or select text, to send it.", { duration: 2000 });
        return true;
      }
      if (paste) {
        sendTextToTerminal(target.text);
        return true;
      }
      const anchor = caretAnchor();
      if (anchor) openSend(target.text, target.block, anchor);
      return true;
    };
  });

  useEffect(() => {
    _codeSendRef.fn = (text, anchor) => {
      if (!editor) return;
      const pos = editor.view.posAtDOM(anchor, 0);
      openSend(text, blockAtPos(editor.state.doc, pos), rectOf(anchor));
    };
    return () => {
      _codeSendRef.fn = null;
    };
  });

  const withContext = useCallback(
    (request: SendRequest) =>
      includeContext && !request.namesNote
        ? `${request.text}\n\n${noteContextLine(noteTitle, noteRef)}`
        : request.text,
    [includeContext, noteTitle, noteRef],
  );

  const sendToAgent = useCallback(
    async (identity: TeamAgentIdentity) => {
      const request = send;
      setSend(null);
      if (!request || !identity.row.sessionId) return;
      // Routed by where the agent runs (this Mac or a machine), never retried here: an
      // uncertain remote delivery may already have reached the agent.
      const outcome = await deliverToAgent(identity, withContext(request));
      if (outcome.kind === "failed") {
        toast.error(outcome.message);
        return;
      }
      if (outcome.kind === "uncertain") {
        toast.warning(outcome.message);
        return;
      }
      // The line now carries the agent it went to. The note may have changed while
      // the text was sent (typing above it, an agent's edit reloading the note), so
      // find the line again by its text; leave it be when it can't be told apart.
      const block = request.block;
      if (editor && !editor.isDestroyed && block) {
        const pos = relocateBlock(editor.state.doc, block);
        if (pos !== null) {
          appendAgentRef(editor, pos, { handle: identity.row.handle, name: identity.label });
        }
      }
    },
    [send, editor, withContext],
  );

  const startNewAgent = useCallback(() => {
    const request = send;
    setSend(null);
    if (!request) return;
    openLaunchSheet({ teamKey, task: withContext(request) });
  }, [send, teamKey, withContext]);

  const pasteToTerminal = useCallback(() => {
    const request = send;
    setSend(null);
    if (request) sendTextToTerminal(request.text);
  }, [send]);

  // ---- hover send button beside a line ----
  const onSheetMouseMove = useCallback(
    (e: React.MouseEvent) => {
      if (!editor || !editor.isEditable || send) return;
      const scroll = scrollRef.current;
      const target = e.target as HTMLElement;
      if (target.closest(".nt-line-send")) return;
      const el = target.closest<HTMLElement>(SENDABLE_LINES);
      if (!el || !scroll || !editor.view.dom.contains(el)) {
        setHoverLine((h) => (h ? null : h));
        return;
      }
      let pos: number;
      try {
        pos = editor.view.posAtDOM(el, 0);
      } catch {
        return;
      }
      const block = blockAtPos(editor.state.doc, pos);
      if (!block || !block.text) {
        setHoverLine((h) => (h ? null : h));
        return;
      }
      const lineEl =
        el.tagName === "LI"
          ? (el.querySelector<HTMLElement>(":scope > div > p, :scope > p") ?? el)
          : el;
      const r = lineEl.getBoundingClientRect();
      const s = scroll.getBoundingClientRect();
      const editorRect = editor.view.dom.getBoundingClientRect();
      const next = {
        pos: block.pos,
        top: r.top - s.top + scroll.scrollTop - 1,
        left: editorRect.right - s.left - 22,
      };
      setHoverLine((h) => (h && h.pos === next.pos && h.top === next.top ? h : next));
    },
    [editor, send],
  );

  // ---- selection menu ----
  const [sel, setSel] = useState<{
    text: string;
    rect: SelectionAnchorRect;
    marks: ActiveMarks;
  } | null>(null);

  const syncSelectionMenu = useCallback(() => {
    if (!editor) return;
    const target = computeSelectionMenu(editor);
    if (!target) {
      setSel(null);
      return;
    }
    const rect = readSelectionRect();
    if (!rect) {
      setSel(null);
      return;
    }
    setSel({ text: target.text, rect, marks: readActiveMarks(editor) });
  }, [editor]);

  useEffect(() => {
    if (!editor) return;
    const dismiss = () => setSel(null);
    editor.on("selectionUpdate", syncSelectionMenu);
    editor.on("blur", dismiss);
    return () => {
      editor.off("selectionUpdate", syncSelectionMenu);
      editor.off("blur", dismiss);
    };
  }, [editor, syncSelectionMenu]);

  useEffect(() => {
    if (!sel) return;
    window.addEventListener("scroll", syncSelectionMenu, true);
    window.addEventListener("resize", syncSelectionMenu);
    return () => {
      window.removeEventListener("scroll", syncSelectionMenu, true);
      window.removeEventListener("resize", syncSelectionMenu);
    };
  }, [sel, syncSelectionMenu]);

  const refreshMarks = useCallback(() => {
    if (!editor) return;
    setSel((s) => (s ? { ...s, marks: readActiveMarks(editor) } : s));
  }, [editor]);

  // SW-SN-9 Task 3: keep the link-click callback pointed at current values.
  useEffect(() => {
    linkClickRef.current = (href) => {
      const result = handleNoteLinkHref(
        href,
        {
          nodes: docNodes,
          teamContentPath,
          teamKey,
          repoPath,
          noteExists: (id) => notes.some((n) => n.id === id),
          setActiveNote,
          openFile: useViewerStore.getState().openFile,
        },
        (url) => {
          openUrl(url).catch(() => toast.error("Couldn't open link"));
        },
      );
      if (result === "broken" && !pipelineLoading) {
        toast("Link target not found", { duration: 2000 });
      }
      return result !== "ignored";
    };
  });

  // ---- notes CRUD ----
  const [deleteTargetId, setDeleteTargetId] = useState<string | null>(null);
  const deleteTarget = notes.find((n) => n.id === deleteTargetId) ?? null;
  const titleRef = useRef<HTMLInputElement>(null);

  const handleCreate = useCallback(
    (title?: string) => {
      setListOpen(false);
      setEmptyDismissed(true);
      void createNote(title)
        .then(() => requestAnimationFrame(() => titleRef.current?.select()))
        .catch(() => toast.error("Could not create note"));
    },
    [createNote],
  );
  const handleSelect = useCallback(
    (id: string) => {
      setListOpen(false);
      return setActiveNote(id);
    },
    [setActiveNote],
  );
  const handleListRename = useCallback(
    (id: string, title: string) => void renameNote(id, title),
    [renameNote],
  );
  const handleRequestDelete = useCallback((id: string) => setDeleteTargetId(id), []);
  const handleTogglePin = useCallback(
    (id: string, pinned: boolean) => void setNotePinned(id, pinned),
    [setNotePinned],
  );

  // The title is part of the sheet: typed in place, saved on Enter or leaving it.
  const [titleDraft, setTitleDraft] = useState(activeNote?.title ?? "");
  const [titleFor, setTitleFor] = useState<{ id: string | null; title: string | undefined }>({
    id: activeNoteId,
    title: activeNote?.title,
  });
  if (titleFor.id !== activeNoteId || titleFor.title !== activeNote?.title) {
    setTitleFor({ id: activeNoteId, title: activeNote?.title });
    setTitleDraft(activeNote?.title ?? "");
  }
  const commitTitle = (text = titleDraft) => {
    if (!activeNote) return;
    const next = text.trim();
    if (next && next !== activeNote.title) void renameNote(activeNote.id, next);
    else setTitleDraft(activeNote.title);
  };

  // ---- headings + outline ----
  const headings = useNoteHeadings(editor, activeNoteId);
  useEffect(() => {
    setActiveNoteHeadings(headings);
  }, [headings, setActiveNoteHeadings]);

  // SW-SN-6: consume the go-to-heading modal's jump (see note-headings).
  useEffect(() => {
    if (!pendingHeadingJump || !editor) return;
    if (pendingHeadingJump.noteId !== activeNoteId) {
      clearHeadingJump();
      return;
    }
    const target = scrollToHeadingBySlug(editor, pendingHeadingJump.slug);
    if (target || headings.length > 0) clearHeadingJump();
  }, [pendingHeadingJump, editor, activeNoteId, headings, clearHeadingJump]);

  // ---- history: who changed this, review, undo ----
  const history = useNoteHistory(activeNoteId);
  const now = useNow(true, 30_000);
  const [review, setReview] = useState<{ rev: NoteRevision; baseline: string } | null>(null);
  const [changedBaseline, setChangedBaseline] = useState<{
    rev: string;
    baseline: string;
  } | null>(null);
  const unseen = history.unseenAgentChange;
  const bannerRev = review?.rev ?? unseen;

  // The body before the unseen agent change, so its lines can be tinted.
  useEffect(() => {
    if (!unseen) return;
    const before = history.revisionBefore(unseen.commit);
    if (!before) return;
    let cancelled = false;
    history.readAt(before.commit).then(
      (baseline) => !cancelled && setChangedBaseline({ rev: unseen.commit, baseline }),
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
    // `history` methods are stable; key on the revision.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [unseen?.commit]);

  const overlay = review
    ? { baseline: review.baseline, review: true }
    : unseen && changedBaseline?.rev === unseen.commit
      ? { baseline: changedBaseline.baseline, review: false }
      : null;
  useEffect(() => {
    if (!editor || editor.isDestroyed) return;
    const current = noteDecorationsKey.getState(editor.state)?.overlay ?? null;
    if (!current && !overlay) return;
    editor.view.dispatch(editor.state.tr.setMeta(noteDecorationsKey, { overlay }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor, overlay?.baseline, overlay?.review]);

  const overlayDiff = useMemo(
    () => (overlay && editor ? diffAgainst(editor.state.doc, overlay.baseline) : null),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [overlay?.baseline, editor, content],
  );
  const removedLines = review && overlayDiff ? overlayDiff.removed : [];
  // The banner counts what the tint shows: changed lines (or, for a pure removal, the
  // lines removed).
  const shownChanges = overlayDiff
    ? overlayDiff.changed.length || overlayDiff.removed.length
    : undefined;

  const startReview = useCallback(
    async (rev: NoteRevision) => {
      const before = history.revisionBefore(rev.commit);
      if (!before) {
        toast("This was the note's first version — nothing to compare.");
        return;
      }
      try {
        const baseline = await history.readAt(before.commit);
        setSheet(null);
        setReview({ rev, baseline });
      } catch {
        toast.error("Couldn't read the earlier version.");
      }
    },
    [history],
  );

  const undoChange = useCallback(
    async (rev: NoteRevision) => {
      try {
        // Only that revision's change is taken back; anything written since stays.
        await history.undo(rev.commit);
        history.markSeen(rev.commit);
        setReview(null);
        toast(`Undid ${rev.actor.name}'s changes.`, { duration: 2500 });
      } catch (e) {
        const why = e instanceof Error ? e.message : typeof e === "string" ? e : "";
        toast.error(why ? `Couldn't undo the change: ${why}` : "Couldn't undo the change.");
      }
    },
    [history],
  );

  const restoreVersion = useCallback(
    async (rev: NoteRevision) => {
      try {
        await history.restore(rev.commit);
        setSheet(null);
        toast("Restored that version.", { duration: 2500 });
      } catch {
        toast.error("Couldn't restore that version.");
      }
    },
    [history],
  );

  const agentChanges = history.entries.filter((e) => e.actor.kind === "agent").length;

  // ---- agent menu ----
  const organize = useCallback(async () => {
    const prompt = await readNotePrompt("organize");
    const context = noteContextLine(noteTitle, noteRef);
    const done = "When you're done, post a short summary of what you changed to my inbox.";
    openLaunchSheet({
      teamKey,
      task: [prompt, context, done].filter(Boolean).join("\n\n"),
    });
  }, [noteTitle, noteRef, teamKey]);

  const sendWholeNote = useCallback(() => {
    const anchor = rootRef.current?.querySelector(".nt-chrome");
    if (!anchor) return;
    const r = rectOf(anchor);
    const at = { top: r.top + r.height - 4, left: r.left + Math.max(8, r.width - 300) };
    openSend(
      `Read my note ${noteContextLine(noteTitle, noteRef).replace(/^The note: /, "")}`,
      null,
      { ...at, width: 0, height: 0 },
      true,
    );
  }, [noteTitle, noteRef, openSend]);

  // ---- empty state ----
  // Nothing written yet: one note, still empty and still carrying its default title.
  const isBlank =
    notes.length <= 1 &&
    content.trim() === "" &&
    scratchNoteLoadStatus !== "pending" &&
    BLANK_TITLES.has(activeNote?.title ?? "");
  const showEmpty = isBlank && !emptyDismissed;

  // ⌘B toggles the list (narrow), ⌘N makes a note — while focus is in the pane.
  const handlePaneKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const mod = (e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey;
      if (!mod) return;
      const key = e.key.toLowerCase();
      if (key === "b") {
        e.stopPropagation();
        if (editor?.isFocused) return; // Bold wins in the editor
        e.preventDefault();
        if (!wide) setListOpen((o) => !o);
      } else if (key === "n") {
        e.preventDefault();
        e.stopPropagation();
        handleCreate();
      }
    },
    [editor, wide, handleCreate],
  );

  const summaries = useNoteListSummaries(notes, activeNoteId, content, wide || listOpen);

  const list = (variant: "column" | "drawer") => (
    <NotesList
      notes={notes}
      activeNoteId={activeNoteId}
      summaries={summaries}
      now={now}
      variant={variant}
      onSelect={handleSelect}
      onCreate={handleCreate}
      onRename={handleListRename}
      onRequestDelete={handleRequestDelete}
      onTogglePin={handleTogglePin}
      onDismiss={variant === "drawer" ? () => setListOpen(false) : undefined}
    />
  );

  const outlineButton =
    headings.length > 0 ? (
      <NtPopover open={outlineOpen} onOpenChange={setOutlineOpen}>
        <NtPopoverTrigger asChild>
          <button
            type="button"
            className="nt-ib"
            aria-label="On this note"
            title={`On this note (${headings.length} heading${headings.length === 1 ? "" : "s"})`}
          >
            <TableOfContents size={16} />
          </button>
        </NtPopoverTrigger>
        <NtPopoverContent align="center" style={{ width: 220 }} aria-label="On this note">
          <div className="nt-pop-label">On this note</div>
          <div onClick={() => setOutlineOpen(false)}>
            <NoteOutline
              editor={editor}
              headings={headings}
              active={outlineOpen}
              noteKey={activeNoteId}
            />
          </div>
          <div className="nt-pop-sep" />
          <button
            type="button"
            className="nt-row"
            onClick={() => {
              setOutlineOpen(false);
              setHeadingJumpOpen(true);
            }}
          >
            <span className="nt-row-icon">
              <Search size={14} />
            </span>
            Go to heading
            <span className="nt-row-sub">
              <kbd className="nt-kbd">⌘⇧O</kbd>
            </span>
          </button>
        </NtPopoverContent>
      </NtPopover>
    ) : null;

  const savedLabel = noteSavedAt
    ? `Saved ${formatAgo(noteSavedAt, Math.max(now, noteSavedAt))}`
    : null;
  const lastAgentEdit = history.entries[0]?.actor.kind === "agent" ? history.entries[0] : null;
  const chromeHeight = 38;
  // History opens under the note's title and meta line, so it stays clear which note
  // it belongs to; when they have scrolled away, it opens under the chrome.
  const [historyTop, setHistoryTop] = useState(chromeHeight + 6);
  const openHistory = useCallback(() => {
    const root = rootRef.current;
    const meta = root?.querySelector(".nt-meta");
    let top = chromeHeight + 6;
    if (root && meta) {
      const below = meta.getBoundingClientRect().bottom - root.getBoundingClientRect().top + 8;
      if (below > top && below < root.clientHeight / 2) top = below;
    }
    setHistoryTop(top);
    setSheet("history");
  }, []);

  return (
    <div
      ref={rootRef}
      className="nt-pane"
      data-layout={wide ? "wide" : "narrow"}
      onKeyDown={handlePaneKeyDown}
    >
      <div className="nt-body">
        {wide && list("column")}
        <div className="nt-main">
          <NotesChrome
            wide={wide}
            listOpen={listOpen}
            outline={outlineButton}
            onToggleList={() => setListOpen((o) => !o)}
            onFind={() => setNoteSwitcherOpen(true)}
            agentMenu={
              <AgentMenu
                disabled={showEmpty}
                actions={{
                  onOrganize: () => void organize(),
                  onSendNote: sendWholeNote,
                  onRoutine: () => setSheet("routine"),
                  onChanges: history.available ? openHistory : undefined,
                  agentChangeCount: agentChanges,
                }}
              />
            }
            noteMenu={
              <NoteActionsMenu
                pinned={activeNote?.pinned ?? false}
                hasNote={activeNote !== null && !showEmpty}
                onNew={() => handleCreate()}
                onTogglePin={() => activeNote && handleTogglePin(activeNote.id, !activeNote.pinned)}
                onRename={() => titleRef.current?.select()}
                onDelete={() => activeNote && setDeleteTargetId(activeNote.id)}
              />
            }
          />
          {bannerRev && history.available && !showEmpty && (
            <ChangeBanner
              rev={bannerRev}
              changeCount={shownChanges}
              reviewing={review !== null}
              now={now}
              onReview={() => void startReview(bannerRev)}
              onUndo={() => void undoChange(bannerRev)}
              onDone={() => {
                history.markSeen(bannerRev.commit);
                setReview(null);
              }}
            />
          )}
          <div className="nt-main" style={{ position: "relative" }}>
            {!showEmpty && (
              <StickyNoteHeaders editor={editor} headings={headings} noteKey={activeNoteId} />
            )}
            <div
              ref={setScrollNode}
              className="nt-scroll"
              onMouseMove={onSheetMouseMove}
              onMouseLeave={() => setHoverLine(null)}
            >
              {showEmpty ? (
                <NotesEmpty
                  orgName={orgName}
                  onNew={() => {
                    setEmptyDismissed(true);
                    requestAnimationFrame(() => titleRef.current?.select());
                  }}
                  onRoutine={() => setSheet("routine")}
                />
              ) : null}
              <div className="nt-sheet" hidden={showEmpty}>
                <input
                  ref={titleRef}
                  className="nt-title"
                  value={titleDraft}
                  placeholder="Untitled"
                  aria-label="Note title"
                  disabled={!activeNote}
                  onChange={(e) => setTitleDraft(e.target.value)}
                  onBlur={(event) => {
                    const field = event.currentTarget;
                    commitTextEditOnBlur(field, () => commitTitle(field.value));
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === "ArrowDown") {
                      e.preventDefault();
                      commitTitle();
                      editor?.commands.focus("start");
                    } else if (e.key === "Escape") {
                      setTitleDraft(activeNote?.title ?? "");
                      e.currentTarget.blur();
                    }
                  }}
                />
                <div className="nt-meta">
                  <span className="nt-meta-item" title="Only you can see your notes">
                    <Lock size={12} aria-hidden />
                    Private
                  </span>
                  <span className="nt-meta-dot" aria-hidden />
                  {lastAgentEdit && history.available ? (
                    <span className="nt-meta-item">
                      {lastAgentEdit.actor.name} edited {formatAgo(lastAgentEdit.at, now)}
                    </span>
                  ) : (
                    <>
                      <span className="nt-meta-item">{orgName}</span>
                      {savedLabel && (
                        <>
                          <span className="nt-meta-dot" aria-hidden />
                          <span className="nt-meta-item" aria-live="polite">
                            {savedLabel}
                          </span>
                        </>
                      )}
                    </>
                  )}
                </div>
                {removedLines.map((line, i) => (
                  <div key={`${i}:${line}`} className="nt-removed" aria-label={`Removed: ${line}`}>
                    {line}
                  </div>
                ))}
                <EditorContent editor={editor} />
              </div>
              {hoverLine && !showEmpty && !sel && !slash && (
                <button
                  type="button"
                  className="nt-line-send"
                  aria-label="Send to… (⌘↵)"
                  title="Send to… ⌘↵"
                  style={{ top: hoverLine.top, left: hoverLine.left }}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={(e) => {
                    if (!editor) return;
                    const block = blockAtPos(editor.state.doc, hoverLine.pos + 1);
                    if (block?.text) openSend(block.text, block, rectOf(e.currentTarget));
                  }}
                >
                  <Send size={14} />
                </button>
              )}
            </div>
          </div>
        </div>
        {wide && !showEmpty && headings.length > 0 && (
          <nav className="nt-toc" aria-label="On this note">
            <div className="nt-toc-label">On this note</div>
            <NoteOutline editor={editor} headings={headings} active noteKey={activeNoteId} />
          </nav>
        )}
      </div>

      {!wide && listOpen && (
        <>
          <div className="nt-scrim" onClick={() => setListOpen(false)} aria-hidden />
          <div className="nt-drawer">{list("drawer")}</div>
        </>
      )}

      {sheet === "routine" && (
        <RoutineSetup
          top={chromeHeight + 6}
          noteTitle={noteTitle}
          note={noteRef}
          onClose={() => setSheet(null)}
        />
      )}
      {sheet === "history" && (
        <HistorySheet
          top={historyTop}
          title={noteTitle}
          entries={history.entries}
          now={now}
          onReview={(rev) => void startReview(rev)}
          onRestore={(rev) => void restoreVersion(rev)}
          onClose={() => setSheet(null)}
        />
      )}

      <SendPicker
        key={send?.seq ?? 0}
        open={send !== null}
        anchor={send?.anchor ?? null}
        placeLabel={orgName}
        includeContext={includeContext}
        onIncludeContextChange={(v) => {
          setIncludeContext(v);
          try {
            localStorage.setItem(INCLUDE_CONTEXT_KEY, String(v));
          } catch {
            // Storage off: the choice holds for this session.
          }
        }}
        onSendToAgent={(identity) => void sendToAgent(identity)}
        onStartNew={startNewAgent}
        onPasteTerminal={pasteToTerminal}
        onClose={() => {
          setSend(null);
          editor?.commands.focus();
        }}
      />

      {editor && (
        <ScratchSelectionMenu
          open={sel !== null && send === null}
          rect={sel?.rect ?? null}
          marks={sel?.marks ?? EMPTY_MARKS}
          onToggleBold={() => {
            editor.chain().focus().toggleBold().run();
            refreshMarks();
          }}
          onToggleItalic={() => {
            editor.chain().focus().toggleItalic().run();
            refreshMarks();
          }}
          onToggleCode={() => {
            editor.chain().focus().toggleCode().run();
            refreshMarks();
          }}
          onLink={() => {
            // Turn the selection into a `[[` query: the picker links a note or a story.
            const { from, to } = editor.state.selection;
            const text = editor.state.doc.textBetween(from, to, " ");
            setSel(null);
            editor.chain().focus().insertContentAt({ from, to }, `[[${text}`).run();
          }}
          onSend={() => {
            if (sel) openSend(sel.text, null, sel.rect);
            setSel(null);
          }}
          onDismiss={() => setSel(null)}
        />
      )}

      {picker && <NoteLinkPicker state={picker} registerKeyHandler={registerPickerKeyHandler} />}
      {slash && <SlashMenu state={slash} registerKeyHandler={registerSlashKeyHandler} />}

      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTargetId(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete “{deleteTarget?.title}”?</DialogTitle>
            <DialogDescription>This note will be permanently removed.</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <AppButton variant="outline" onClick={() => setDeleteTargetId(null)}>
              Cancel
            </AppButton>
            <AppButton
              variant="destructive"
              onClick={() => {
                if (deleteTargetId) void deleteNote(deleteTargetId);
                setDeleteTargetId(null);
              }}
            >
              Delete
            </AppButton>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
