// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { attachmentHandlers } from "./attachments";
import { resolveScenario } from "../scenario";
import type { Attachment } from "@/shared/types/domain";
import type { ParsedGitRemote } from "@/tauri/commands";

describe("attachmentHandlers", () => {
  it("resolves an attachment (clone on disk) when the scenario has one attached", () => {
    const h = attachmentHandlers(resolveScenario("?scenario=fresh"));
    const byRemote = h.list_attachments_by_remote({ githubUserId: 4242 }) as Record<
      string,
      Attachment
    >;
    // Keyed by lowercase owner/repo, with a repo_path that becomes localPath.
    expect(byRemote["acme/widgets"].repo_path).toBeTruthy();
    expect(h.resolve_source_repo_attachment({})).not.toBeNull();
  });

  it("resolves no attachment (empty map) when the scenario's clone is unattached", () => {
    const h = attachmentHandlers(resolveScenario("?scenario=noAttachedClone"));
    expect(h.list_attachments_by_remote({ githubUserId: 4242 })).toEqual({});
    expect(h.resolve_source_repo_attachment({})).toBeNull();
  });

  it("parses an https github remote into host/provider/owner/repo", () => {
    const h = attachmentHandlers(resolveScenario("?scenario=fresh"));
    const parsed = h.parse_git_remote({
      url: "https://github.com/acme/widgets.git",
    }) as ParsedGitRemote;
    expect(parsed).toEqual({
      host: "github.com",
      provider: "github",
      owner: "acme",
      repo: "widgets",
    });
  });

  it("parses an scp-style ssh remote and infers a non-github provider from the host", () => {
    const h = attachmentHandlers(resolveScenario("?scenario=fresh"));
    const parsed = h.parse_git_remote({ url: "git@gitlab.com:team/proj.git" }) as ParsedGitRemote;
    expect(parsed).toEqual({
      host: "gitlab.com",
      provider: "gitlab",
      owner: "team",
      repo: "proj",
    });
  });
});
