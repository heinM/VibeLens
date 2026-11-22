import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { FileCode2, FileText, FolderOpen, FolderInput, RefreshCw, SquareTerminal, X } from "lucide-react";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { useWorkspaceFiles } from "../../hooks/useWorkspaceFiles";
import { api, asError } from "../../ipc/commands";
import type { ErrorPayload } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { importWorkspaceFiles, useImportNoticeStore } from "../../lib/workspaceImport";
import { ErrorBox, IconButton } from "../ui";
import { SkeletonRows } from "../ui/wsKit";
import { SectionHeader } from "./TreeRow";

function iconFor(path: string) {
  if (/\.sql$/i.test(path)) return <FileCode2 className="size-3.5 text-sky-500" />;
  if (/\.(valkey|redis)$/i.test(path)) return <SquareTerminal className="size-3.5 text-rose-400" />;
  return <FileText className="size-3.5 text-muted" />;
}

export function WorkspaceFiles({ collapsed = false, onToggleCollapsed }: { collapsed?: boolean; onToggleCollapsed?: () => void }) {
  const files = useWorkspaceFiles();
  const openWorkspaceFile = useWorkspaceStore((s) => s.openWorkspaceFile);
  const tabs = useWorkspaceStore((s) => s.tabs);
  const openPaths = tabs.map((t) => t.path);
  const activeProfileId = useUIStore((s) => s.activeProfileId);
  const [openError, setOpenError] = useState<ErrorPayload | null>(null);
  const qc = useQueryClient();
  const [importing, setImporting] = useState(false);
  const [revealError, setRevealError] = useState<ErrorPayload | null>(null);
  const notice = useImportNoticeStore((s) => s.notice);
  const setNotice = useImportNoticeStore((s) => s.setNotice);

  const importFiles = () => {
    setImporting(true);
    setRevealError(null);
    void importWorkspaceFiles(qc, activeProfileId).finally(() => setImporting(false));
  };
  const reveal = () => {
    setRevealError(null);
    api.wsReveal().catch((e: unknown) => setRevealError(asError(e)));
  };

  const list = [...(files.data ?? [])].sort((a, b) => a.path.localeCompare(b.path, undefined, { numeric: true, sensitivity: "base" }));

  return (
    <div className="flex h-full min-h-0 flex-col bg-panel">
      <SectionHeader
        title="Workspace files"
        count={files.data?.length}
        collapsed={collapsed}
        onToggleCollapsed={onToggleCollapsed}
        actions={
          !collapsed && (
            <>
              <IconButton label="Import files…" onClick={importFiles} disabled={importing}>
                <FolderInput className="size-3.5" />
              </IconButton>
              <IconButton label="Reveal folder" onClick={reveal}>
                <FolderOpen className="size-3.5" />
              </IconButton>
              <IconButton label="Refresh workspace files" onClick={() => void files.refetch()}>
                <RefreshCw className={cn("size-3.5", files.isFetching && "animate-spin")} />
              </IconButton>
            </>
          )
        }
      />
      {!collapsed && <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {files.isLoading && <SkeletonRows rows={3} />}
        {files.error && (
          <div className="p-2">
            <ErrorBox error={asError(files.error)} />
          </div>
        )}
        {openError && (
          <div className="p-2">
            <ErrorBox error={openError} />
          </div>
        )}
        {revealError && (
          <div className="p-2">
            <ErrorBox error={revealError} onDismiss={() => setRevealError(null)} />
          </div>
        )}
        {notice && (
          <div className="p-2">
            <div role="status" className="selectable rounded-md border border-border bg-panel-2 px-3 py-2 text-xs text-muted">
              <div className="flex items-start gap-2">
                <p className="min-w-0 flex-1 text-fg">
                  {notice.imported > 0 ? `Imported ${notice.imported} file${notice.imported === 1 ? "" : "s"}.` : "Nothing was imported."}
                  {notice.skipped.length > 0 && ` Skipped ${notice.skipped.length}:`}
                </p>
                <IconButton label="Dismiss" className="-mr-1 -mt-0.5 size-5 shrink-0" onClick={() => setNotice(null)}>
                  <X className="size-3.5" aria-hidden />
                </IconButton>
              </div>
              {notice.skipped.length > 0 && (
                <ul className="mt-1 space-y-0.5">
                  {notice.skipped.map((k, i) => (
                    <li key={i} className="break-words">
                      <span className="text-fg">{k.name || "(unnamed)"}</span>: {k.reason}
                    </li>
                  ))}
                </ul>
              )}
              {notice.error && (
                <div className="mt-1.5">
                  <ErrorBox error={notice.error} />
                </div>
              )}
            </div>
          </div>
        )}
        {files.data && list.length === 0 && (
          <p className="px-3 py-3 text-xs leading-relaxed text-muted">Saved worksheets appear here. Press the save shortcut in a SQL tab to create one.</p>
        )}
        {list.map((f) => {
          const slash = f.path.lastIndexOf("/");
          const dir = slash >= 0 ? f.path.slice(0, slash + 1) : "";
          const name = slash >= 0 ? f.path.slice(slash + 1) : f.path;
          const isOpen = openPaths.includes(f.path);
          return (
            <button
              key={f.path}
              type="button"
              title={f.path}
              onClick={() => {
                setOpenError(null);
                openWorkspaceFile(f.path, activeProfileId).catch((e: unknown) => setOpenError(asError(e)));
              }}
              className={cn("flex h-6 w-full items-center gap-2 pl-4 pr-2 text-left text-[13px] outline-none hover:bg-panel-2", isOpen ? "text-fg" : "text-fg/85")}
            >
              {iconFor(f.path)}
              <span className="min-w-0 truncate">
                {dir && <span className="text-muted">{dir}</span>}
                {name}
              </span>
              {isOpen && <span aria-label="open" className="ml-auto size-1.5 shrink-0 rounded-full bg-accent" />}
            </button>
          );
        })}
      </div>}
    </div>
  );
}
