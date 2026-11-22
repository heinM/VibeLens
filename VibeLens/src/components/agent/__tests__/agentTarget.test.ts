// The agent's target (pinning), the run_sql card actions, opening agent SQL in a tab, and the
// stick-to-bottom decision. Pure logic plus the zustand stores, with the IPC layer mocked.
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  agentCancel: vi.fn(async (_id: string) => undefined),
  agentReset: vi.fn(async (_id: string) => undefined),
  listProfiles: vi.fn(async () => []),
  wsRead: vi.fn(async (_p: string) => ""),
}));
vi.mock("../../../ipc/commands", () => ({ api, asError: (e: unknown) => e }));
// In-memory localStorage so the persist middleware writes somewhere we can read.
const storage = vi.hoisted(() => {
  const m = new Map<string, string>();
  const s = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
  const g = globalThis as { localStorage?: unknown; window?: unknown };
  g.localStorage = s;
  g.window ??= { localStorage: s };
  return s;
});

import { currentAgentTarget, pinStillValid, resolveAgentTarget } from "../../../hooks/useAgentContext";
import { useAgentStore } from "../../../store/useAgentStore";
import { useUIStore } from "../../../store/useUIStore";
import { useWorkspaceStore } from "../../../store/useWorkspaceStore";
import { consumeAutoRun, consumeAutoRunReadOnly } from "../../editor/autoRun";
import { openAgentTab } from "../openInTab";
import { sqlRunActions, toolIconName } from "../toolSummary";
import { pinAfterScroll, REPIN_DISTANCE_PX } from "../useStickToBottom";
import type { SqlRun } from "../../../ipc/types";

const sqlTab = { kind: "sql" as const, profileId: "tab-conn", database: "tab-db" };

beforeEach(() => {
  useAgentStore.setState({ pinnedTarget: null });
  useWorkspaceStore.setState({ tabs: [], activeTabId: null });
  useUIStore.setState({ activeProfileId: "side-conn", activeDatabase: "side-db" });
});

describe("resolveAgentTarget", () => {
  it("follows focus while unpinned: the active tab, else the sidebar", () => {
    expect(resolveAgentTarget(null, sqlTab, "side-conn", "side-db")).toEqual({ profileId: "tab-conn", database: "tab-db" });
    expect(resolveAgentTarget(null, null, "side-conn", "side-db")).toEqual({ profileId: "side-conn", database: "side-db" });
  });
  it("uses the pinned target whatever has focus", () => {
    const pinned = { profileId: "pinned-conn", database: "payments" };
    expect(resolveAgentTarget(pinned, sqlTab, "side-conn", "side-db")).toEqual(pinned);
    expect(resolveAgentTarget(pinned, null, null, null)).toEqual(pinned);
    // A pin on a connection's default database stays on the default, not on the focused one.
    expect(resolveAgentTarget({ profileId: "pinned-conn", database: null }, sqlTab, "side-conn", "side-db")).toEqual({ profileId: "pinned-conn", database: null });
  });
  it("reads the stores at call time", () => {
    useWorkspaceStore.getState().openTab({ ...sqlTab });
    expect(currentAgentTarget()).toEqual({ profileId: "tab-conn", database: "tab-db" });
    useAgentStore.getState().pinTarget({ profileId: "pinned-conn", database: "payments" });
    useWorkspaceStore.getState().openTab({ kind: "sql", profileId: "other", database: "elsewhere" });
    expect(currentAgentTarget()).toEqual({ profileId: "pinned-conn", database: "payments" });
    useAgentStore.getState().unpin();
    expect(currentAgentTarget()).toEqual({ profileId: "other", database: "elsewhere" });
  });
});

describe("pin state", () => {
  it("survives a new session", () => {
    useAgentStore.getState().pinTarget({ profileId: "p1", database: "shop" });
    const before = useAgentStore.getState().sessionId;
    useAgentStore.getState().reset();
    expect(useAgentStore.getState().sessionId).not.toBe(before);
    expect(useAgentStore.getState().pinnedTarget).toEqual({ profileId: "p1", database: "shop" });
  });
  it("is not persisted", () => {
    useAgentStore.getState().pinTarget({ profileId: "p1", database: "shop" });
    const saved = JSON.parse(storage.getItem("vibelens.agent") ?? "null");
    expect(saved.state).toEqual({ defaultProviderId: null });
  });
  it("is stale only once a loaded profile list lacks the connection", () => {
    const pin = { profileId: "p1", database: null };
    expect(pinStillValid(null, [])).toBe(true);
    expect(pinStillValid(pin, undefined)).toBe(true);
    expect(pinStillValid(pin, [{ id: "p1" }, { id: "p2" }])).toBe(true);
    expect(pinStillValid(pin, [{ id: "p2" }])).toBe(false);
    expect(pinStillValid(pin, [])).toBe(false);
  });
});

describe("run_sql card actions", () => {
  const ran = (readOnly: boolean): SqlRun => ({ profileId: "p1", database: "payments", sql: "select 1", readOnly });
  it("offers Open for any executed run_sql and Run only when it ran read-only", () => {
    expect(sqlRunActions({ tool: "run_sql", sqlRun: ran(true) })).toEqual({ open: true, run: true });
    expect(sqlRunActions({ tool: "run_sql", sqlRun: ran(false) })).toEqual({ open: true, run: false });
    expect(sqlRunActions({ tool: "run_sql", sqlRun: null })).toEqual({ open: false, run: false });
    expect(sqlRunActions({ tool: "get_schema", sqlRun: ran(true) })).toEqual({ open: false, run: false });
  });
  it("does not offer Run for a call that failed", () => {
    expect(sqlRunActions({ tool: "run_sql", sqlRun: ran(true), status: "error" })).toEqual({ open: true, run: false });
    expect(sqlRunActions({ tool: "run_sql", sqlRun: ran(true), status: "ok" })).toEqual({ open: true, run: true });
  });
  it("shows list_databases with the list icon", () => {
    expect(toolIconName("list_databases")).toBe("list");
  });
});

describe("openAgentTab", () => {
  it("opens a dirty SQL tab on the given target and only queues a run when asked", () => {
    const id = openAgentTab({ kind: "sql", profileId: "p1", database: "payments", contents: "select 1" });
    const tab = useWorkspaceStore.getState().tabs.find((t) => t.id === id);
    expect(tab).toMatchObject({ kind: "sql", profileId: "p1", database: "payments", contents: "select 1", dirty: true, title: "agent.sql" });
    expect(useWorkspaceStore.getState().activeTabId).toBe(id);
    expect(consumeAutoRun(id)).toBe(false);

    const runId = openAgentTab({ kind: "sql", profileId: "p1", database: null, contents: "select 2", run: true });
    expect(consumeAutoRun(runId)).toBe(true);
    // The re-run stays read-only, and the flag is used once.
    expect(consumeAutoRunReadOnly(runId)).toBe(true);
    expect(consumeAutoRunReadOnly(runId)).toBe(false);
  });
  it("never binds a Valkey tab to a database or runs it", () => {
    const id = openAgentTab({ kind: "valkey", profileId: "v1", database: "x", contents: "GET k", run: true });
    expect(useWorkspaceStore.getState().tabs.find((t) => t.id === id)).toMatchObject({ kind: "valkey", database: null, title: "agent.valkey" });
    expect(consumeAutoRun(id)).toBe(false);
  });
});

describe("pinAfterScroll", () => {
  const far = REPIN_DISTANCE_PX + 400;
  it("ignores scrolls the user did not make, even far from the bottom", () => {
    expect(pinAfterScroll(true, { byUser: false, movedUp: true, distanceFromBottom: far })).toBe(true);
    expect(pinAfterScroll(false, { byUser: false, movedUp: false, distanceFromBottom: 0 })).toBe(false);
  });
  it("lets go when the user scrolls up away from the bottom", () => {
    expect(pinAfterScroll(true, { byUser: true, movedUp: true, distanceFromBottom: far })).toBe(false);
  });
  it("does not let go when the user scrolls down but is still far away", () => {
    expect(pinAfterScroll(true, { byUser: true, movedUp: false, distanceFromBottom: far })).toBe(true);
    expect(pinAfterScroll(false, { byUser: true, movedUp: false, distanceFromBottom: far })).toBe(false);
  });
  it("re-pins when the user comes back near the bottom", () => {
    expect(pinAfterScroll(false, { byUser: true, movedUp: false, distanceFromBottom: REPIN_DISTANCE_PX })).toBe(true);
    expect(pinAfterScroll(true, { byUser: true, movedUp: true, distanceFromBottom: 40 })).toBe(true);
  });
});
