// Shared helpers for opening object tabs. Re-focuses an existing tab for the same object.
import { useWorkspaceStore } from "../../store/useWorkspaceStore";

export function openTableTab(profileId: string, database: string | null, schema: string, name: string): string {
  const ws = useWorkspaceStore.getState();
  const existing = ws.tabs.find(
    (t) => t.kind === "table" && t.profileId === profileId && t.database === database && t.objectRef?.schema === schema && t.objectRef?.name === name,
  );
  if (existing) {
    ws.setActive(existing.id);
    return existing.id;
  }
  return ws.openTab({ kind: "table", title: name, profileId, database, objectRef: { schema, name } });
}

export function openErdTab(profileId: string, database: string | null, schema: string): string {
  const ws = useWorkspaceStore.getState();
  const existing = ws.tabs.find((t) => t.kind === "erd" && t.profileId === profileId && t.database === database && t.objectRef?.schema === schema);
  if (existing) {
    ws.setActive(existing.id);
    return existing.id;
  }
  return ws.openTab({ kind: "erd", title: `${schema} diagram`, profileId, database, objectRef: { schema, name: "" } });
}
