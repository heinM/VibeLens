import { describe, expect, it, vi } from "vitest";
import { runWorkspaceImport, type ImportDeps, type ImportNotice } from "../workspaceImport";
import type { WorkspaceImportOutcome } from "../../ipc/types";

function setup(result: WorkspaceImportOutcome | null | { fail: unknown }, openFile: ImportDeps["openFile"] = async () => undefined) {
  const notices: Array<ImportNotice | null> = [];
  const deps: ImportDeps = {
    pick: vi.fn(async () => {
      if (result && "fail" in result) throw result.fail;
      return result;
    }),
    refreshList: vi.fn(),
    openFile: vi.fn(openFile),
    setNotice: (n) => void notices.push(n),
  };
  return { deps, notices };
}

describe("runWorkspaceImport", () => {
  it("does nothing when the picker is cancelled", async () => {
    const { deps, notices } = setup(null);
    await runWorkspaceImport(deps);
    expect(deps.refreshList).not.toHaveBeenCalled();
    expect(deps.openFile).not.toHaveBeenCalled();
    expect(notices).toEqual([]);
  });

  it("refreshes the list and opens a single imported file", async () => {
    const { deps, notices } = setup({ imported: ["a-2.sql"], skipped: [] });
    await runWorkspaceImport(deps);
    expect(deps.refreshList).toHaveBeenCalledTimes(1);
    expect(deps.openFile).toHaveBeenCalledWith("a-2.sql");
    expect(notices).toEqual([null]);
  });

  it("opens nothing when several files were imported", async () => {
    const { deps } = setup({ imported: ["a.sql", "b.sql"], skipped: [] });
    await runWorkspaceImport(deps);
    expect(deps.refreshList).toHaveBeenCalledTimes(1);
    expect(deps.openFile).not.toHaveBeenCalled();
  });

  it("reports what was skipped and why", async () => {
    const skipped = [{ name: "x.exe", reason: "file type not allowed" }];
    const { deps, notices } = setup({ imported: ["a.sql", "b.sql"], skipped });
    await runWorkspaceImport(deps);
    expect(notices).toEqual([{ imported: 2, skipped, error: null }]);
  });

  it("turns a failed pick or a failed open into a notice instead of throwing", async () => {
    const failed = setup({ fail: { code: "invalid", message: "boom" } });
    await runWorkspaceImport(failed.deps);
    expect(failed.notices[0]?.error?.message).toBe("boom");
    expect(failed.deps.refreshList).not.toHaveBeenCalled();

    const open = setup({ imported: ["a.sql"], skipped: [] }, async () => {
      throw new Error("cannot open");
    });
    await runWorkspaceImport(open.deps);
    expect(open.deps.refreshList).toHaveBeenCalled();
    expect(open.notices[0]).toMatchObject({ imported: 1, error: { message: "cannot open" } });
  });
});
