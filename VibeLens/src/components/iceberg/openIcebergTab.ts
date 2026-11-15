// Open (or focus) the Iceberg table tab. Namespace levels are joined with "." in objectRef.schema;
// a level containing "." is escaped as "\." so namespaceLevels() can split it back.
import { useWorkspaceStore } from "../../store/useWorkspaceStore";

export const joinNamespace = (levels: string[]) => levels.map((l) => l.replace(/\\/g, "\\\\").replace(/\./g, "\\.")).join(".");

export function namespaceLevels(joined: string): string[] {
  const out: string[] = [];
  let cur = "";
  for (let i = 0; i < joined.length; i++) {
    const c = joined[i];
    if (c === "\\" && i + 1 < joined.length) cur += joined[++i];
    else if (c === ".") (out.push(cur), (cur = ""));
    else cur += c;
  }
  out.push(cur);
  return out.filter((l, i) => l !== "" || i < out.length - 1);
}

export function openIcebergTableTab(profileId: string, namespace: string[], name: string): string {
  const ws = useWorkspaceStore.getState();
  const schema = joinNamespace(namespace);
  const existing = ws.tabs.find((t) => t.kind === "icetable" && t.profileId === profileId && t.objectRef?.schema === schema && t.objectRef?.name === name);
  if (existing) {
    ws.setActive(existing.id);
    return existing.id;
  }
  return ws.openTab({ kind: "icetable", title: name, profileId, database: null, objectRef: { schema, name } });
}
