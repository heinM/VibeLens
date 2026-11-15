import { Database, FileCode2, SquareTerminal } from "lucide-react";
import { useProfiles } from "../../hooks/useProfiles";
import { useTabActions } from "../../hooks/useTabActions";
import { useUIStore } from "../../store/useUIStore";
import { Button, Spinner } from "../ui";
import { Kbd } from "../ui/wsKit";
import { MOD } from "../ui/platform";

/** Shown when no tabs are open: points at the next sensible action. */
export function EmptyWorkspace() {
  const { data: profiles, isLoading } = useProfiles();
  const openWizard = useUIStore((s) => s.openWizard);
  const { newSqlTab, newValkeyConsole } = useTabActions();
  const hasProfiles = (profiles?.length ?? 0) > 0;

  return (
    <div className="flex h-full items-center justify-center bg-bg p-8">
      <div className="w-full max-w-md">
        <img src="/vibelens.svg" alt="" className="mb-5 size-10" />
        {isLoading ? (
          <Spinner />
        ) : !hasProfiles ? (
          <>
            <h2 className="text-lg font-semibold text-fg">Connect to a database</h2>
            <p className="mt-1.5 text-[13px] leading-relaxed text-muted">
              Add a PostgreSQL or Valkey connection. Passwords go straight into the OS keychain, and production connections ask before they change anything.
            </p>
            <div className="mt-5">
              <Button variant="primary" onClick={() => openWizard()}>
                <Database className="size-4" /> New connection
              </Button>
            </div>
          </>
        ) : (
          <>
            <h2 className="text-lg font-semibold text-fg">Open a worksheet</h2>
            <p className="mt-1.5 text-[13px] leading-relaxed text-muted">Double-click a table in the navigator to browse it, or start from a blank worksheet.</p>
            <div className="mt-5 flex flex-wrap gap-2">
              <Button variant="primary" onClick={() => newSqlTab()}>
                <FileCode2 className="size-4" /> New SQL tab
              </Button>
              <Button variant="secondary" onClick={() => newValkeyConsole()}>
                <SquareTerminal className="size-4" /> New Valkey console
              </Button>
            </div>
          </>
        )}
        <dl className="mt-8 grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-2 text-xs text-muted">
          <dt className="flex gap-1"><Kbd>{MOD}</Kbd><Kbd>N</Kbd></dt>
          <dd>New SQL tab</dd>
          <dt className="flex gap-1"><Kbd>{MOD}</Kbd><Kbd>↵</Kbd></dt>
          <dd>Run the selection or the statement at the cursor</dd>
          <dt className="flex gap-1"><Kbd>{MOD}</Kbd><Kbd>B</Kbd></dt>
          <dd>Show or hide the navigator</dd>
          <dt className="flex gap-1"><Kbd>{MOD}</Kbd><Kbd>J</Kbd></dt>
          <dd>Show or hide the agent panel</dd>
        </dl>
      </div>
    </div>
  );
}
