// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";

import { readFile, writeContentFile } from "@/tauri/commands";

import { relateDoc, unrelateDoc, RELATE_TOAST } from "./relate-doc";

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, readFile: vi.fn(), writeContentFile: vi.fn() };
});

const toastSuccess = vi.fn();
const toastError = vi.fn();
const toastBare = vi.fn();
vi.mock("sonner", () => ({
  toast: Object.assign((msg: string) => toastBare(msg), {
    success: (msg: string) => toastSuccess(msg),
    error: (msg: string) => toastError(msg),
  }),
}));

const readFileMock = readFile as Mock;
const writeMock = writeContentFile as Mock;

beforeEach(() => {
  vi.clearAllMocks();
  writeMock.mockResolvedValue(undefined);
});

describe("relateDoc (SLW-FX-21 shared orchestration)", () => {
  it("attaches into the TARGET md and toasts success when the write changes the doc", async () => {
    readFileMock.mockResolvedValue(`---\ntype: story\n---\nbody\n`);
    await relateDoc("/ctx/epic/story.md", "/ctx/epic/design.md");
    expect(writeMock).toHaveBeenCalledTimes(1);
    expect(writeMock.mock.calls[0][0]).toBe("/ctx/epic/story.md");
    expect(writeMock.mock.calls[0][1]).toContain("supporting_docs:\n  - ./design.md");
    expect(toastSuccess).toHaveBeenCalledWith(RELATE_TOAST.added);
  });

  it("toasts 'Already related' and writes nothing when the doc is already listed", async () => {
    readFileMock.mockResolvedValue(`---\nsupporting_docs:\n  - ./design.md\n---\n`);
    await relateDoc("/ctx/epic/story.md", "/ctx/epic/design.md");
    expect(writeMock).not.toHaveBeenCalled();
    expect(toastBare).toHaveBeenCalledWith(RELATE_TOAST.already);
    expect(toastSuccess).not.toHaveBeenCalled();
  });

  it("toasts the add-error copy and never throws on IO failure", async () => {
    readFileMock.mockRejectedValue(new Error("disk gone"));
    await expect(relateDoc("/ctx/epic/story.md", "/ctx/epic/x.md")).resolves.toBeUndefined();
    expect(toastError).toHaveBeenCalledWith(RELATE_TOAST.addError);
  });
});

describe("unrelateDoc (SLW-FX-21 shared orchestration)", () => {
  it("detaches from the TARGET md and toasts 'Unlinked' when the write changes the doc", async () => {
    readFileMock.mockResolvedValue(`---\nsupporting_docs:\n  - ./design.md\n  - ./notes.md\n---\n`);
    await unrelateDoc("/ctx/epic/story.md", "/ctx/epic/design.md");
    expect(writeMock).toHaveBeenCalledTimes(1);
    expect(writeMock.mock.calls[0][1]).not.toContain("design.md");
    expect(toastSuccess).toHaveBeenCalledWith(RELATE_TOAST.unlinked);
  });

  it("is silent and writes nothing when the doc wasn't related (changed:false)", async () => {
    readFileMock.mockResolvedValue(`---\nsupporting_docs:\n  - ./notes.md\n---\n`);
    await unrelateDoc("/ctx/epic/story.md", "/ctx/epic/absent.md");
    expect(writeMock).not.toHaveBeenCalled();
    expect(toastSuccess).not.toHaveBeenCalled();
    expect(toastError).not.toHaveBeenCalled();
    expect(toastBare).not.toHaveBeenCalled();
  });

  it("toasts the unlink-error copy and never throws on IO failure", async () => {
    readFileMock.mockRejectedValue(new Error("disk gone"));
    await expect(unrelateDoc("/ctx/epic/story.md", "/ctx/epic/x.md")).resolves.toBeUndefined();
    expect(toastError).toHaveBeenCalledWith(RELATE_TOAST.unlinkError);
  });
});

describe("relateDoc serialization (no lost writes onto one target)", () => {
  it("serializes concurrent relates onto the SAME target so neither write is lost", async () => {
    // A stateful in-memory file: each read returns the CURRENT content, each write
    // updates it. Without serialization both reads would see the original and the
    // second write would clobber the first.
    let content = `---\ntype: story\n---\nbody\n`;
    readFileMock.mockImplementation(async () => content);
    writeMock.mockImplementation(async (_path: string, text: string) => {
      content = text;
    });

    await Promise.all([
      relateDoc("/ctx/epic/story.md", "/ctx/epic/a.md"),
      relateDoc("/ctx/epic/story.md", "/ctx/epic/b.md"),
    ]);

    // Both relations survived — the second read saw the first's write.
    expect(content).toContain("./a.md");
    expect(content).toContain("./b.md");
    expect(writeMock).toHaveBeenCalledTimes(2);
  });

  it("runs relates onto DIFFERENT targets independently (still both written)", async () => {
    const store: Record<string, string> = {
      "/ctx/epic/one.md": `---\ntype: story\n---\n`,
      "/ctx/epic/two.md": `---\ntype: story\n---\n`,
    };
    readFileMock.mockImplementation(async (p: string) => store[p]);
    writeMock.mockImplementation(async (p: string, text: string) => {
      store[p] = text;
    });

    await Promise.all([
      relateDoc("/ctx/epic/one.md", "/ctx/epic/x.md"),
      relateDoc("/ctx/epic/two.md", "/ctx/epic/y.md"),
    ]);

    expect(store["/ctx/epic/one.md"]).toContain("./x.md");
    expect(store["/ctx/epic/two.md"]).toContain("./y.md");
  });
});
