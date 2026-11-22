// Runs before src/main.tsx (the screenshot Vite config injects it ahead of the app entry).
// Sets the per-scene starting state the app reads at start-up (localStorage, as a user's earlier
// session would have left it, and which connections the stand-in backend reports as connected),
// installs the stand-in backend, and once the app has rendered, hands over to the scene driver.
//
// It imports nothing from src/ that has side effects, so the app's stores are first created by
// the app itself, after this has run.
import { installMock, mockState } from "./mock-ipc";
import { ID, SSO_EXPIRED_DETAIL } from "./demo-data";
import { SCENES, type SceneName } from "./scene-list";

const name = (new URLSearchParams(location.search).get("scene") ?? "overview") as SceneName;
const scene = SCENES[name];

try {
  localStorage.clear();
} catch {
  /* nothing stored */
}

/** react-resizable-panels' saved layout (percentages), as the app's layoutStorage keeps it. */
function layout(id: string, sizes: Record<string, number>) {
  localStorage.setItem(`vg.layout.react-resizable-panels:${id}:${Object.keys(sizes).join(":")}`, JSON.stringify(sizes));
}

localStorage.setItem("vg.theme", "dark");
localStorage.setItem("vg.aiMode", scene?.aiMode ?? "assist");
localStorage.setItem("vg.agentOpen", scene?.agentOpen === false ? "0" : "1");
localStorage.setItem("vibelens.agent", JSON.stringify({ state: { defaultProviderId: "anthropic" }, version: 1 }));
localStorage.setItem("vg.sidebar.historyCollapsed", "1");
layout("vg-main", { sidebar: scene?.sidebarPct ?? 19, center: 100 - (scene?.sidebarPct ?? 19) });
layout("vg-sidebar", scene?.sidebarRows ?? { profiles: 32, explorer: 48, files: 20 });
if (scene?.agentPct) {
  const beside = scene.aiMode === "agentFirst";
  layout(beside ? "vg-center-beside" : "vg-center", { work: 100 - scene.agentPct, agent: scene.agentPct });
}
if (scene?.editorPct) layout("vg-sql-split", { editor: scene.editorPct, results: 100 - scene.editorPct });

for (const id of scene?.connected ?? [ID.shopPg, ID.shopMy, ID.cache, ID.trino, ID.redshift]) mockState.connected.add(id);
if (scene?.athenaExpired !== false) mockState.failed.set(ID.athena, SSO_EXPIRED_DETAIL);

installMock();

window.addEventListener("load", () => {
  void import("./scenes").then((m) => m.run(name));
});
