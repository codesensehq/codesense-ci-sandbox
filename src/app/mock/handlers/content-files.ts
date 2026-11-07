// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { MockState } from "../mock-state";
import type {
  FileEntry,
  LandingCandidate,
  LandingPageResolution,
  StoryDetails,
} from "@/tauri/commands";

function extensionOf(name: string): string | null {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : null;
}

function fileEntry(path: string, content: string): FileEntry {
  const name = path.slice(path.lastIndexOf("/") + 1);
  return { name, path, is_dir: false, extension: extensionOf(name), size: content.length };
}

function dirEntry(path: string): FileEntry {
  const name = path.slice(path.lastIndexOf("/") + 1);
  return { name, path, is_dir: true, extension: null, size: null };
}

/** Project the flat path→content store into a single directory listing: the
 *  files directly under `dirPath` plus one synthetic entry per immediate
 *  subdirectory. */
function listDirectory(state: MockState, dirPath: string): FileEntry[] {
  const prefix = dirPath === "" ? "" : `${dirPath.replace(/\/$/, "")}/`;
  const entries: FileEntry[] = [];
  const seenDirs = new Set<string>();

  for (const file of state.contentFiles.list()) {
    if (!file.path.startsWith(prefix)) continue;
    const rest = file.path.slice(prefix.length);
    const slash = rest.indexOf("/");
    if (slash === -1) {
      entries.push(fileEntry(file.path, file.content));
    } else {
      const dirName = rest.slice(0, slash);
      if (!seenDirs.has(dirName)) {
        seenDirs.add(dirName);
        entries.push(dirEntry(`${prefix}${dirName}`));
      }
    }
  }
  return entries;
}

export function contentFileHandlers(state: MockState): HandlerRegistry {
  return {
    write_content_file: async (args) => {
      const path = args.path as string;
      // The editor's lock, as the real writer honours it.
      const expected = args.expectedSha256 as string | undefined;
      const current = state.contentFiles.get(path)?.content;
      if (expected === "absent" && current != null) {
        throw `conflict: ${path} was created since it was deleted`;
      }
      if (
        expected &&
        expected !== "absent" &&
        (current == null || (await sha256(current)) !== expected)
      ) {
        throw `conflict: ${path} changed on disk since it was opened`;
      }
      state.contentFiles.put({ id: path, path, content: args.content as string });
      return undefined;
    },

    delete_content_file: (args) => {
      const removed = state.contentFiles.remove(args.path as string);
      if (!removed) throw new Error(`file not found: ${args.path as string}`);
      return undefined;
    },

    move_content_file: (args) => {
      const fromPath = args.fromPath as string;
      const toPath = args.toPath as string;
      const file = state.contentFiles.get(fromPath);
      if (!file) throw new Error(`file not found: ${fromPath}`);
      if (toPath !== fromPath && state.contentFiles.has(toPath)) {
        throw new Error(`destination already exists: ${toPath}`);
      }
      state.contentFiles.remove(fromPath);
      state.contentFiles.put({ id: toPath, path: toPath, content: file.content });
      return undefined;
    },

    list_directory: (args) => listDirectory(state, args.dirPath as string),

    // Mirrors Rust `team_landing::resolve` (the mock has no symlinks).
    resolve_team_landing_page: (args): LandingPageResolution => {
      const root = (args.contentRoot as string).replace(/\/+$/, "");
      const prefix = `${root}/`;
      const files = state.contentFiles.list().filter((f) => f.path.startsWith(prefix));
      if (files.length === 0) return { kind: "unavailable" };
      for (const c of args.candidates as LandingCandidate[]) {
        const want = `${prefix}${c.path}`;
        const hit =
          files.find((f) => f.path === want) ??
          (c.anyCase ? files.find((f) => f.path.toLowerCase() === want.toLowerCase()) : undefined);
        if (hit) return { kind: "found", path: hit.path };
      }
      return { kind: "none" };
    },

    // R-14 / SW-LN-34: the lazy card-title fetch, read from the stored content's
    // 4 KB head like the real command. Section text isn't modelled in mock mode.
    get_story_details: (args) => {
      const file = state.contentFiles.get(args.filePath as string);
      if (!file) throw new Error(`file not found: ${args.filePath as string}`);
      return storyDetailsFromText(file.path, file.content.slice(0, 4096).replace(/\r\n/g, "\n"));
    },
  };
}

function storyDetailsFromText(path: string, head: string): StoryDetails {
  const clean = (value: string | undefined) => value?.trim() || null;
  if (/\.html?$/i.test(path)) {
    const meta = head.match(/<meta\s+name="cs:title"\s+content="([^"]*)"/i)?.[1];
    const title =
      head.match(/<title>([^<]*)<\/title>/i)?.[1] ?? head.match(/<h1[^>]*>([^<]*)<\/h1>/i)?.[1];
    return { title: clean(title), storyText: null, titleMeta: clean(meta) };
  }
  const frontmatter = head.match(/^---\n([\s\S]*?)\n---/)?.[1] ?? "";
  const rawMeta = frontmatter.match(/^title:[ \t]*(.*)$/m)?.[1]?.trim();
  const meta = rawMeta?.replace(/^(["'])(.*)\1$/, "$2");
  const body = head.slice(frontmatter ? head.indexOf("\n---", 4) + 4 : 0);
  const h1 = body.match(/^#[ \t]+(.+)$/m)?.[1];
  return { title: clean(h1), storyText: null, titleMeta: clean(meta) };
}

async function sha256(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}
