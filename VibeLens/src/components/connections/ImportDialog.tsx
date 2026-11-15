import { useCallback, useEffect, useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, ChevronRight, ClipboardPaste, FileUp, KeyRound, RefreshCw } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { ErrorPayload, ImportCandidate, ImportReport, ImportSaveResult, ImportSource } from "../../ipc/types";
import { PROFILES_KEY } from "../../hooks/useProfiles";
import { cn } from "../../lib/cn";
import { useUIStore } from "../../store/useUIStore";
import { Badge, Button, Dialog, EnvBadge, ErrorBox, Spinner } from "../ui";
import { Notice, Switch } from "../ui/formKit";

export const SOURCE_LABEL: Record<ImportSource, string> = {
  dataGrip: "DataGrip / IntelliJ",
  dBeaver: "DBeaver",
  azureDataStudio: "Azure Data Studio",
  pgAdmin: "pgAdmin 4",
  pgService: "pg_service.conf",
};

const ENGINE_LABEL = { postgres: "Postgres", valkey: "Valkey", iceberg: "Iceberg" } as const;

/** Stable key for a candidate across re-renders (the backend gives each a fresh profile id). */
const keyOf = (c: ImportCandidate) => c.profile.id;

/** `host:port/db` the way people read it. */
export function endpointOf(c: ImportCandidate): string {
  const p = c.profile;
  const db = p.engine === "postgres" && p.database ? `/${p.database}` : "";
  return `${p.host}:${p.port}${db}`;
}

/** The name to save: optionally prefixed with the source folder (`Prod / orders`). */
export function importName(c: ImportCandidate, prefixWithGroup: boolean): string {
  const g = c.group?.trim();
  return prefixWithGroup && g ? `${g} / ${c.profile.name}` : c.profile.name;
}

/** Which candidates start ticked: everything new; nothing that already exists. */
export function initialSelection(reports: ImportReport[]): Set<string> {
  const s = new Set<string>();
  for (const r of reports) for (const c of r.candidates) if (!c.duplicateOf) s.add(keyOf(c));
  return s;
}

function userOf(c: ImportCandidate): string | null {
  const a = c.profile.auth;
  return "username" in a && a.username ? a.username : null;
}

export function ImportDialog() {
  const open = useUIStore((s) => s.importOpen);
  const setOpen = useUIStore((s) => s.setImportOpen);
  const qc = useQueryClient();
  const [reports, setReports] = useState<ImportReport[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<ErrorPayload | null>(null);
  const [prefix, setPrefix] = useState(false);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [result, setResult] = useState<ImportSaveResult | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  /** Adds reports (scan, file, paste), replacing any earlier report for the same path. */
  const merge = useCallback((incoming: ImportReport[]) => {
    setReports((prev) => {
      const paths = new Set(incoming.map((r) => r.path));
      return [...prev.filter((r) => !paths.has(r.path) || r.path === "pasted text"), ...incoming];
    });
    setSelected((prev) => {
      const next = new Set(prev);
      for (const k of initialSelection(incoming)) next.add(k);
      return next;
    });
  }, []);

  const scan = useCallback(async () => {
    setScanning(true);
    setError(null);
    try {
      const found = await api.importScan();
      setReports([]);
      setSelected(new Set());
      merge(found);
    } catch (e) {
      setError(asError(e));
    } finally {
      setScanning(false);
    }
  }, [merge]);

  useEffect(() => {
    if (!open) return;
    setResult(null);
    setPasteOpen(false);
    setPasteText("");
    void scan();
  }, [open, scan]);

  const chooseFile = async () => {
    setError(null);
    try {
      const r = await api.importChooseFile();
      if (r) merge([r]);
    } catch (e) {
      setError(asError(e));
    }
  };

  const parsePaste = async () => {
    setError(null);
    try {
      merge([await api.importText(pasteText)]);
      setPasteOpen(false);
      setPasteText("");
    } catch (e) {
      setError(asError(e));
    }
  };

  const all = useMemo(() => reports.flatMap((r) => r.candidates), [reports]);
  const chosen = all.filter((c) => selected.has(keyOf(c)));
  const needSecrets = chosen.filter((c) => c.needsSecret).length;

  const toggle = (k: string) =>
    setSelected((s) => {
      const n = new Set(s);
      if (n.has(k)) n.delete(k);
      else n.add(k);
      return n;
    });
  const toggleReport = (r: ImportReport, on: boolean) =>
    setSelected((s) => {
      const n = new Set(s);
      for (const c of r.candidates) {
        if (on) n.add(keyOf(c));
        else n.delete(keyOf(c));
      }
      return n;
    });

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const res = await api.importSave(chosen.map((c) => ({ profile: { ...c.profile, name: importName(c, prefix) }, source: c.source })));
      setResult(res);
      await qc.invalidateQueries({ queryKey: PROFILES_KEY });
    } catch (e) {
      setError(asError(e));
    } finally {
      setSaving(false);
    }
  };

  const close = () => setOpen(false);

  const footer = result ? (
    <Button variant="primary" onClick={close}>
      Done
    </Button>
  ) : (
    <>
      <Button variant="ghost" onClick={close} disabled={saving}>
        Cancel
      </Button>
      <Button variant="primary" onClick={() => void save()} loading={saving} disabled={chosen.length === 0}>
        Import {chosen.length > 0 ? chosen.length : ""} connection{chosen.length === 1 ? "" : "s"}
      </Button>
    </>
  );

  return (
    <Dialog open={open} onClose={close} title="Import connections" width="max-w-3xl" footer={footer}>
      {result ? (
        <div className="space-y-3">
          <Notice tone={result.failed.length ? "warn" : "info"}>
            Imported {result.saved.length} connection{result.saved.length === 1 ? "" : "s"}.
            {needSecrets > 0 && <> Add passwords by editing each connection (right-click → Edit connection…): passwords are never copied from other tools.</>}
          </Notice>
          {result.failed.length > 0 && (
            <ul className="space-y-1 text-[12px]">
              {result.failed.map((f) => (
                <li key={f.name} className="text-danger">
                  {f.name}: {f.error}
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : (
        <div className="space-y-3">
          <p className="text-[12px] leading-snug text-muted">
            VibeLens looked in the usual places for DataGrip and other JetBrains IDEs, DBeaver, Azure Data Studio, pgAdmin (servers.json export) and pg_service.conf. Postgres and Redis/Valkey connections can be imported.
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <Button size="sm" onClick={() => void scan()} loading={scanning}>
              <RefreshCw className="size-3.5" /> Scan again
            </Button>
            <Button size="sm" onClick={() => void chooseFile()}>
              <FileUp className="size-3.5" /> Choose file…
            </Button>
            <Button size="sm" onClick={() => setPasteOpen((o) => !o)}>
              <ClipboardPaste className="size-3.5" /> Paste…
            </Button>
            <span className="flex-1" />
            <Switch checked={prefix} onChange={setPrefix} label="Prefix names with folder" />
          </div>

          {pasteOpen && (
            <div className="space-y-2">
              <textarea
                aria-label="Pasted connections"
                value={pasteText}
                onChange={(e) => setPasteText(e.target.value)}
                spellCheck={false}
                rows={6}
                placeholder="In DataGrip select data sources and press Copy (⌘C / Ctrl+C), then paste here. The contents of a DBeaver, Azure Data Studio, pgAdmin or pg_service.conf file work too."
                className="w-full rounded-md border border-border bg-bg p-2 font-mono text-[12px] outline-none focus:border-accent"
              />
              <Button size="sm" variant="primary" disabled={!pasteText.trim()} onClick={() => void parsePaste()}>
                Read pasted text
              </Button>
            </div>
          )}

          <ErrorBox error={error} />

          {scanning && reports.length === 0 ? (
            <div className="flex items-center gap-2 py-6 text-[12px] text-muted">
              <Spinner className="size-3.5" /> Looking for connections…
            </div>
          ) : reports.length === 0 ? (
            <p className="py-4 text-[12px] text-muted">
              Nothing found in the usual places. Use <span className="font-medium text-fg">Choose file…</span> for a dataSources.xml, data-sources.json, settings.json, servers.json or pg_service.conf, or paste DataGrip&rsquo;s copied data sources.
            </p>
          ) : (
            <div className="space-y-3">
              {reports.map((r) => {
                const allOn = r.candidates.length > 0 && r.candidates.every((c) => selected.has(keyOf(c)));
                return (
                  <section key={r.path} className="rounded-lg border border-border">
                    <header className="flex items-center gap-2 border-b border-border px-3 py-2">
                      {r.candidates.length > 0 && (
                        <input type="checkbox" aria-label={`Select all from ${r.path}`} checked={allOn} onChange={(e) => toggleReport(r, e.target.checked)} />
                      )}
                      <span className="text-[12.5px] font-medium">{SOURCE_LABEL[r.source]}</span>
                      <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted" title={r.path}>
                        {r.path}
                      </span>
                      <span className="shrink-0 text-[11px] text-muted">
                        {r.candidates.length} found{r.skipped.length ? `, ${r.skipped.length} skipped` : ""}
                      </span>
                    </header>
                    {r.error && <p className="px-3 py-2 text-[12px] text-danger">{r.error}</p>}
                    <ul>
                      {r.candidates.map((c) => {
                        const k = keyOf(c);
                        const user = userOf(c);
                        const open = expanded.has(k);
                        return (
                          <li key={k} className="border-b border-border/60 last:border-b-0">
                            <div className="flex items-center gap-2 px-3 py-1.5">
                              <input type="checkbox" aria-label={`Import ${c.profile.name}`} checked={selected.has(k)} onChange={() => toggle(k)} />
                              <button
                                type="button"
                                className="flex min-w-0 flex-1 items-center gap-2 text-left"
                                onClick={() => setExpanded((s) => {
                                  const n = new Set(s);
                                  if (n.has(k)) n.delete(k);
                                  else n.add(k);
                                  return n;
                                })}
                              >
                                <ChevronRight className={cn("size-3 shrink-0 text-muted transition-transform", open && "rotate-90", c.warnings.length === 0 && "invisible")} />
                                <span className="truncate text-[12.5px]">{importName(c, prefix)}</span>
                                <Badge>{ENGINE_LABEL[c.profile.engine]}</Badge>
                                <EnvBadge env={c.profile.environment} />
                                <span className="min-w-0 truncate font-mono text-[11px] text-muted">
                                  {user ? `${user}@` : ""}
                                  {endpointOf(c)}
                                </span>
                              </button>
                              {c.profile.tunnel && <Badge>SSH</Badge>}
                              {c.needsSecret && (
                                <span title="Needs a password after import" className="text-muted">
                                  <KeyRound className="size-3.5" />
                                </span>
                              )}
                              {c.duplicateOf && <Badge className="border-warn/40 text-warn">Already added</Badge>}
                              {c.warnings.length > 0 && (
                                <span title={c.warnings.join("\n")} className="text-warn">
                                  <AlertTriangle className="size-3.5" />
                                </span>
                              )}
                            </div>
                            {open && c.warnings.length > 0 && (
                              <ul className="space-y-0.5 pb-2 pl-12 pr-3 text-[11.5px] text-muted">
                                {c.warnings.map((w) => (
                                  <li key={w}>• {w}</li>
                                ))}
                              </ul>
                            )}
                          </li>
                        );
                      })}
                    </ul>
                    {r.skipped.length > 0 && (
                      <details className="px-3 py-2 text-[11.5px] text-muted">
                        <summary className="cursor-default">Skipped ({r.skipped.length})</summary>
                        <ul className="mt-1 space-y-0.5">
                          {r.skipped.map((s, i) => (
                            <li key={`${s.name}-${i}`}>
                              <span className="text-fg">{s.name}</span>: {s.reason}
                            </li>
                          ))}
                        </ul>
                      </details>
                    )}
                  </section>
                );
              })}
            </div>
          )}

          {chosen.length > 0 && (
            <Notice tone="info">
              Passwords are not imported: the other tools keep them in your keychain or their own encrypted stores. {needSecrets > 0 ? `${needSecrets} of these need one; add it by editing the connection after import.` : ""} Imported connections get AI access <span className="font-medium">schema only</span>; review it per connection.
            </Notice>
          )}
        </div>
      )}
    </Dialog>
  );
}
