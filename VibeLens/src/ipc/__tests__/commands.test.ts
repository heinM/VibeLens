// The IPC wrappers for the new Tauri commands: exact command names and argument names, because the
// Rust side (docs/REMOTE_ENGINES_AND_REDSHIFT.md, section 4.3) is written against the same contract.
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => {
  class Channel<T> {
    onmessage: ((m: T) => void) | null = null;
  }
  return { invoke: vi.fn(async (_cmd: string, _args?: unknown) => undefined as unknown), Channel };
});
vi.mock("@tauri-apps/api/core", () => tauri);

import { api } from "../commands";
import { newProfile } from "../../lib/defaults";

const lastCall = () => tauri.invoke.mock.calls[tauri.invoke.mock.calls.length - 1] as [string, Record<string, unknown>];

beforeEach(() => tauri.invoke.mockClear());

describe("save_profile engineSecret", () => {
  it("sends engineSecret: null to keep, \"\" to delete, a value to store", async () => {
    const p = newProfile("iceberg");
    await api.saveProfile(p);
    expect(lastCall()[0]).toBe("save_profile");
    expect(lastCall()[1]).toMatchObject({ engineSecret: null, icebergSecret: null, password: null });
    await api.saveProfile(p, undefined, undefined, undefined, undefined, false, "");
    expect(lastCall()[1].engineSecret).toBe("");
    await api.saveProfile(p, undefined, undefined, undefined, undefined, false, "tok");
    expect(lastCall()[1].engineSecret).toBe("tok");
  });
  it("sends the Redshift options inside the profile", async () => {
    const p = newProfile("redshift");
    await api.saveProfile(p);
    expect((lastCall()[1].profile as { redshift: unknown }).redshift).toEqual(p.redshift);
  });
});

describe("Redshift commands", () => {
  it("use the contract's command and argument names", async () => {
    await api.rsListDatabases("p1");
    expect(lastCall()).toEqual(["rs_list_databases", { profileId: "p1" }]);
    await api.rsListSchemas("p1", null);
    expect(lastCall()).toEqual(["rs_list_schemas", { profileId: "p1", database: null }]);
    await api.rsListTables("p1", "dev", "public");
    expect(lastCall()).toEqual(["rs_list_tables", { profileId: "p1", database: "dev", schema: "public" }]);
    await api.rsDescribeTable("p1", null, "public", "orders");
    expect(lastCall()).toEqual(["rs_describe_table", { profileId: "p1", database: null, schema: "public", name: "orders" }]);
    await api.rsSchemaMetadata("p1", "dev");
    expect(lastCall()).toEqual(["rs_schema_metadata", { profileId: "p1", database: "dev", maxTables: null }]);
    await api.rsSchemaMetadata("p1", null, 500);
    expect(lastCall()[1].maxTables).toBe(500);
  });
  it("rs_execute mirrors ice_execute plus the database, with full options and a channel", async () => {
    const onEvent = vi.fn();
    await api.rsExecute({ profileId: "p1", database: "dev", sql: "select 1", options: { readOnly: true }, onEvent });
    const [cmd, args] = lastCall();
    expect(cmd).toBe("rs_execute");
    expect(Object.keys(args).sort()).toEqual(["confirmed", "database", "onEvent", "options", "profileId", "sql"]);
    expect(args).toMatchObject({ profileId: "p1", database: "dev", sql: "select 1", confirmed: false, options: { readOnly: true, maxRows: 100_000, pageSize: 500, timeoutMs: null } });
    expect(args.onEvent).toBeInstanceOf(tauri.Channel);
    (args.onEvent as { onmessage: (m: unknown) => void }).onmessage({ type: "finished", elapsedMs: 1 });
    expect(onEvent).toHaveBeenCalledWith({ type: "finished", elapsedMs: 1 });
  });
});
