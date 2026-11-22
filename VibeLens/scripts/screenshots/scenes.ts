// Drives the running app into each screenshot's state, the way a user would get there: focus a
// connection, expand the tree, open a tab and run it, type to the agent, open a dialog. It uses
// the app's own stores and DOM; nothing in src/ is changed. Sets window.__sceneDone (or
// window.__sceneError) when the scene is ready to capture.
import { useUIStore } from "../../src/store/useUIStore";
import { useWorkspaceStore } from "../../src/store/useWorkspaceStore";
import { useAgentStore } from "../../src/store/useAgentStore";
import { useInspectorStore } from "../../src/components/valkey/inspectorStore";
import { requestAutoRun } from "../../src/components/editor/autoRun";
import { ID, TOP_CUSTOMERS, ice, my } from "./demo-data";
import type { SceneName } from "./scene-list";

declare global {
  interface Window {
    __sceneDone?: boolean;
    __sceneError?: string;
  }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function waitFor<T>(what: string, fn: () => T | null | undefined | false, timeoutMs = 15000): Promise<T> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await sleep(50);
  }
}

const textOf = (el: Element) => (el.textContent ?? "").replace(/\s+/g, " ").trim();

/** The first element matching `selector` whose text is `text` (or matches it). */
function find(selector: string, text: string | RegExp, root: ParentNode = document): HTMLElement | null {
  for (const el of Array.from(root.querySelectorAll<HTMLElement>(selector))) {
    const t = textOf(el);
    if (typeof text === "string" ? t === text : text.test(t)) return el;
  }
  return null;
}
const waitText = (selector: string, text: string | RegExp, root?: ParentNode) => waitFor(`${selector} "${String(text)}"`, () => find(selector, text, root));

/** A tree row by the text of its label (the first label span after the chevron and icon). */
function treeRow(label: string, tree?: string): HTMLElement | null {
  const scope = tree ? document.querySelector(`[role=tree][aria-label="${tree}"]`) : document;
  if (!scope) return null;
  for (const el of Array.from(scope.querySelectorAll<HTMLElement>("[role=treeitem]"))) {
    const spans = Array.from(el.querySelectorAll(":scope > span"));
    if (spans.some((s) => textOf(s) === label)) return el;
  }
  return null;
}
const waitRow = (label: string, tree?: string) => waitFor(`tree row "${label}"`, () => treeRow(label, tree));

/** Expands a collapsed tree row through its chevron (a no-op when already expanded). */
async function expand(label: string, tree?: string) {
  const row = await waitRow(label, tree);
  if (row.getAttribute("aria-expanded") === "true") return;
  (row.querySelector(":scope > span") as HTMLElement).click();
  await waitFor(`"${label}" expanded`, () => treeRow(label, tree)?.getAttribute("aria-expanded") === "true");
}
async function collapse(label: string, tree?: string) {
  const row = await waitRow(label, tree);
  if (row.getAttribute("aria-expanded") !== "true") return;
  (row.querySelector(":scope > span") as HTMLElement).click();
  await waitFor(`"${label}" collapsed`, () => treeRow(label, tree)?.getAttribute("aria-expanded") === "false");
}

function focus(profileId: string, database: string | null = null) {
  useUIStore.getState().setActiveTarget(profileId, database);
}

/** Opens a SQL tab bound to a connection and runs it once its editor has mounted. */
function openAndRun(t: { title: string; profileId: string; database: string | null; contents: string; path?: string }): string {
  const id = `tab-${t.title}`;
  requestAutoRun(id);
  useWorkspaceStore.getState().openTab({ id, kind: "sql", title: t.title, profileId: t.profileId, database: t.database, contents: t.contents, path: t.path ?? null, dirty: false });
  return id;
}

/** Waits until the results grid shows `cellText` (so the run finished and painted). */
const waitCell = (cellText: string) => waitText("[role=gridcell], td, div", cellText);

async function waitEditor(snippet: string) {
  await waitFor("editor text", () => Array.from(document.querySelectorAll(".monaco-editor .view-lines")).some((v) => textOf(v).includes(snippet)));
}

/** Types into the agent composer and sends, as a user would (Enter). */
async function sendToAgent(text: string) {
  const ta = await waitFor("agent composer", () => document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message to the AI agent"]'));
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  setter?.call(ta, text);
  ta.dispatchEvent(new Event("input", { bubbles: true }));
  await sleep(100);
  ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", bubbles: true, cancelable: true }));
}

function blur() {
  (document.activeElement as HTMLElement | null)?.blur?.();
}

// ---------------------------------------------------------------------------------------------
// SQL shown in the editors (each matches a result in data/*.json, whitespace aside)
// ---------------------------------------------------------------------------------------------

const BY_STATUS = `SELECT status, count(*) AS orders, sum(total) AS revenue
FROM shop.orders
GROUP BY status
ORDER BY revenue DESC;`;

// ---------------------------------------------------------------------------------------------
// Scenes
// ---------------------------------------------------------------------------------------------

/** Shop (Postgres) focused with the shop schema open instead of the empty public schema. */
async function shopTree() {
  focus(ID.shopPg, "vibelens_test");
  await waitRow("vibelens_test");
  await waitRow("public");
  await collapse("public");
  await expand("shop");
  await waitRow("customers");
}

const SCENE_FNS: Record<SceneName, () => Promise<void>> = {
  async overview() {
    await shopTree();
    openAndRun({ title: "top-customers.sql", path: "reports/top-customers.sql", profileId: ID.shopPg, database: "vibelens_test", contents: TOP_CUSTOMERS });
    await waitEditor("Best customers");
    await waitCell("Pieter Naidoo");
  },

  async postgres() {
    await shopTree();
    await expand("order_events");
    await waitFor("partition summary", () => find("[role=treeitem]", /pg_partman/));
    openAndRun({ title: "orders-by-status.sql", profileId: ID.shopPg, database: "vibelens_test", contents: BY_STATUS });
    await waitEditor("revenue");
    await waitCell("shipped");
  },

  async mysql() {
    focus(ID.shopMy, "shop");
    await expand("shop");
    await expand("customers");
    await waitRow("profile");
    openAndRun({ title: "customers.sql", profileId: ID.shopMy, database: "shop", contents: my.customers.sql.replace(" FROM", "\nFROM").replace(" ORDER", "\nORDER") });
    await waitEditor("customers");
    await waitCell("Nomsa Dlamini");
  },

  async valkey() {
    focus(ID.cache);
    useWorkspaceStore.getState().openTab({ id: "tab-console", kind: "valkey", title: "console.valkey", profileId: ID.cache, valkeyDb: 0, contents: "" });
    await waitRow("db0");
    await expand("config");
    const key = await waitRow("app");
    key.click();
    await waitFor("inspector", () => useInspectorStore.getState().target);
    await waitText("span, div", /af-south-1/);
  },

  async iceberg() {
    focus(ID.trino);
    await expand("shop");
    await expand("orders");
    await waitRow("placed_at");
    openAndRun({ title: "revenue-by-country.sql", profileId: ID.trino, database: null, contents: ice.revenueByCountry.sql });
    await waitEditor("revenue");
    await waitText("[data-testid=result-stats] *", "Trino");
  },

  async icebergEngines() {
    focus(ID.trino);
    useUIStore.getState().openWizard(ID.trino);
    const nav = await waitText("button", /^Query engine/);
    nav.click();
    await waitText("*", /Local \(DataFusion\)/);
  },

  async redshift() {
    focus(ID.redshift, "dev");
    await expand("shop");
    await expand("orders");
    await waitRow("placed_at");
    openAndRun({ title: "top-customers.sql", profileId: ID.redshift, database: null, contents: TOP_CUSTOMERS });
    await waitEditor("Best customers");
    await waitText("[data-testid=result-stats] *", "Redshift");
  },

  async newConnection() {
    focus(ID.shopPg, "vibelens_test");
    useUIStore.getState().openWizard();
    await waitText("*", "Amazon Redshift");
  },

  async aiAccess() {
    focus(ID.shopPg, "vibelens_test");
    useUIStore.getState().openWizard();
    const nav = await waitText("button", /^AI access/);
    nav.click();
    await waitText("*", /Results, with approval/);
  },

  async agent() {
    await shopTree();
    useAgentStore.getState().pinTarget({ profileId: ID.shopPg, database: "vibelens_test" });
    await sendToAgent("Who are our best customers by spend? Leave out cancelled orders.");
    await waitFor("turn finished", () => !useAgentStore.getState().running && useAgentStore.getState().transcript.length > 3, 20000);
    const runInTab = await waitText("button", "Run in tab");
    runInTab.click();
    await waitEditor("last_order");
    await waitCell("Pieter Naidoo");
  },

  async approval() {
    await shopTree();
    useAgentStore.getState().pinTarget({ profileId: ID.shopPg, database: "vibelens_test" });
    openAndRun({ title: "top-customers.sql", path: "reports/top-customers.sql", profileId: ID.shopPg, database: "vibelens_test", contents: TOP_CUSTOMERS });
    await waitCell("Pieter Naidoo");
    await sendToAgent("Cancel the orders that have been pending for more than two weeks.");
    await waitText("button", /^Approve/);
  },

  async providers() {
    focus(ID.shopPg, "vibelens_test");
    useUIStore.getState().setSettingsOpen(true);
    const nav = await waitText("button", /^(AI )?[Pp]roviders/);
    nav.click();
    const ollama = await waitText("button", /^Ollama \(local\)/);
    ollama.click();
    await waitText("label", /Context window/);
    ollama.scrollIntoView({ block: "start" });
  },

  async sso() {
    focus(ID.athena);
    await waitText("button", /Sign in again/);
  },
};

export async function run(name: SceneName) {
  try {
    await waitFor("app shell", () => document.querySelector("header"));
    const fn = SCENE_FNS[name];
    if (!fn) throw new Error(`no scene ${name}`);
    await fn();
    blur();
    await sleep(600); // let fonts, Monaco tokenisation and the grid settle
    window.__sceneDone = true;
  } catch (e) {
    window.__sceneError = e instanceof Error ? e.message : String(e);
  }
}
