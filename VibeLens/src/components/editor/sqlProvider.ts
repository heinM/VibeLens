// Monaco glue for lib/sqlIntel: one completion provider and one hover provider for `pgsql`,
// registered once for the whole app. Each editor model is mapped to its tab's (profile, database)
// so the provider can find the cached catalog for whichever editor is asking.
//
// Performance contract: the provider only ever *reads* the TanStack cache on the hot path. A fetch
// happens when the cache is empty (first use), at most once per 30 s on failure, and in the
// background when the cached catalog is older than its stale time. Nothing here debounces typing.
import type { QueryClient } from "@tanstack/react-query";
import type * as MonacoNS from "monaco-editor";
import type { SchemaMetadata } from "../../ipc/types";
import { fetchSchemaMetadata, schemaMetadataKey } from "../../lib/schemaCache";
import { type Catalog, indexCatalog } from "../../lib/sqlIntel/catalog";
import { type Suggestion, complete } from "../../lib/sqlIntel/complete";
import { type SqlDialect, catalogEngineOf } from "../../lib/sqlIntel/dialect";
import { hoverAt } from "../../lib/sqlIntel/hover";

type Monaco = typeof MonacoNS;

export interface ModelTarget {
  profileId: string | null;
  /** Postgres database; unused for Iceberg. */
  database: string | null;
  /** Postgres when omitted. Decides the catalog source (pg vs Iceberg), functions and keywords. */
  dialect?: SqlDialect | null;
  /** Iceberg: the connection's default namespace (dotted), so tables in it complete without a prefix. */
  defaultSchema?: string | null;
}

const keyFor = (t: ModelTarget) => schemaMetadataKey(t.profileId, t.database, catalogEngineOf(t.dialect));

/** The catalog index for a fetched `SchemaMetadata` under this target's dialect. */
export function catalogOf(meta: SchemaMetadata, t: ModelTarget): Catalog {
  return catalogEngineOf(t.dialect) === "iceberg" ? indexCatalog(meta, { dotted: true, defaultSchema: t.defaultSchema ?? null }) : indexCatalog(meta);
}

const STALE_MS = 5 * 60_000;
const FAILURE_BACKOFF_MS = 30_000;
/** First-ever completion waits this long for the catalog before falling back to keywords. */
const FIRST_FETCH_WAIT_MS = 1500;

const targets = new Map<string, ModelTarget>();
const failedAt = new Map<string, number>();
let installed = false;
let queryClient: QueryClient | null = null;

export function setModelTarget(modelUri: string, target: ModelTarget | null): void {
  if (target && target.profileId) targets.set(modelUri, target);
  else targets.delete(modelUri);
}

export function clearModelTarget(modelUri: string): void {
  targets.delete(modelUri);
}

const keyOf = (t: ModelTarget) => JSON.stringify(keyFor(t));

function fetchCatalog(qc: QueryClient, t: ModelTarget): Promise<SchemaMetadata | null> {
  const k = keyOf(t);
  const last = failedAt.get(k);
  if (last && Date.now() - last < FAILURE_BACKOFF_MS) return Promise.resolve(null);
  return qc
    .fetchQuery({
      queryKey: keyFor(t),
      queryFn: () => fetchSchemaMetadata(catalogEngineOf(t.dialect), t.profileId as string, t.database),
      staleTime: STALE_MS,
    })
    .then((m) => {
      failedAt.delete(k);
      return m;
    })
    .catch(() => {
      failedAt.set(k, Date.now());
      return null;
    });
}

/** Warms the catalog for a target (called when an editor mounts or its target changes). */
export function warmCatalog(t: ModelTarget): void {
  if (queryClient && t.profileId) void fetchCatalog(queryClient, t);
}

/** The catalog if cached (sync), else a promise that resolves within a bounded wait. */
function catalogFor(t: ModelTarget | undefined): Catalog | null | Promise<Catalog | null> {
  const qc = queryClient;
  if (!qc || !t || !t.profileId) return null;
  const key = keyFor(t);
  const data = qc.getQueryData<SchemaMetadata>(key);
  if (data) {
    const state = qc.getQueryState(key);
    // Older than the stale time, or invalidated by DDL: refresh behind the scenes, serve what we have.
    if (state && (state.isInvalidated || Date.now() - state.dataUpdatedAt > STALE_MS)) void fetchCatalog(qc, t);
    return catalogOf(data, t);
  }
  const wait = new Promise<null>((r) => setTimeout(() => r(null), FIRST_FETCH_WAIT_MS));
  return Promise.race([fetchCatalog(qc, t).then((m) => (m ? catalogOf(m, t) : null)), wait]);
}

function kindOf(monaco: Monaco, k: Suggestion["kind"]): MonacoNS.languages.CompletionItemKind {
  const K = monaco.languages.CompletionItemKind;
  switch (k) {
    case "column":
      return K.Field;
    case "table":
      return K.Class;
    case "view":
      return K.Interface;
    case "schema":
      return K.Module;
    case "cte":
      return K.Reference;
    case "alias":
      return K.Variable;
    case "keyword":
      return K.Keyword;
    case "function":
      return K.Function;
    case "snippet":
      return K.Snippet;
    case "join":
      return K.Event;
    case "type":
      return K.TypeParameter;
  }
}

export function installSqlIntel(monaco: Monaco, qc: QueryClient): void {
  queryClient = qc;
  if (installed) return;
  installed = true;

  monaco.languages.registerCompletionItemProvider("pgsql", {
    // "@" and "$" open the Iceberg hints after a table name: "orders@<snapshot>", "orders$snapshots".
    triggerCharacters: [".", " ", ",", '"', "@", "$"],
    provideCompletionItems(model, position, context) {
      const target = targets.get(model.uri.toString());
      const build = (cat: Catalog | null): MonacoNS.languages.CompletionList => {
        const text = model.getValue();
        const offset = model.getOffsetAt(position);
        const res = complete(cat, text, offset, { triggerCharacter: context.triggerCharacter, dialect: target?.dialect ?? null });
        if (!res) return { suggestions: [] };
        const a = model.getPositionAt(res.replaceStart);
        const b = model.getPositionAt(res.insertEnd);
        const c = model.getPositionAt(res.replaceEnd);
        const range = {
          insert: new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column),
          replace: new monaco.Range(a.lineNumber, a.column, c.lineNumber, c.column),
        };
        return {
          suggestions: res.suggestions.map((s) => ({
            label: { label: s.label, detail: s.description ? `  ${s.description}` : undefined, description: s.detail },
            kind: kindOf(monaco, s.kind),
            detail: s.detail,
            documentation: s.documentation ? { value: s.documentation } : undefined,
            insertText: s.insertText,
            insertTextRules: s.isSnippet ? monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet : undefined,
            filterText: s.filterText,
            sortText: s.sortText,
            preselect: s.preselect,
            range,
            command: s.retrigger ? { id: "editor.action.triggerSuggest", title: "" } : undefined,
          })),
        };
      };
      const cat = catalogFor(target);
      return cat instanceof Promise ? cat.then(build) : build(cat);
    },
  });

  monaco.languages.registerHoverProvider("pgsql", {
    provideHover(model, position) {
      const target = targets.get(model.uri.toString());
      const cat = catalogFor(target);
      // Hover never waits on the network: no cached catalog, no card.
      if (!cat || cat instanceof Promise) return null;
      const info = hoverAt(cat, model.getValue(), model.getOffsetAt(position));
      if (!info) return null;
      const a = model.getPositionAt(info.start);
      const b = model.getPositionAt(info.end);
      return { range: new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column), contents: [{ value: info.markdown }] };
    },
  });
}
