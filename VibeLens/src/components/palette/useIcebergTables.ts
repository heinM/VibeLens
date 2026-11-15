// The table list behind "Iceberg: open table…": fetched when the picker opens, cached briefly.
import { useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { ConnectionProfile } from "../../ipc/types";
import { asError } from "../../ipc/commands";
import { openIcebergTableTab } from "../iceberg/openIcebergTab";
import { defaultIcebergTableActions, icebergTableItems, listIcebergTables } from "./icebergPalette";
import type { PaletteItem } from "./items";

const ACTIONS = defaultIcebergTableActions(openIcebergTableTab);

export interface IcebergTablePicker {
  items: PaletteItem[];
  loading: boolean;
  error: string | null;
  truncated: boolean;
}

/** Lists the connection's tables across namespaces while `profile` is set (the picker is open). */
export function useIcebergTablePicker(profile: ConnectionProfile | undefined): IcebergTablePicker {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["ice", profile?.id ?? "", "palette-tables"],
    queryFn: () => listIcebergTables(qc, profile?.id as string),
    enabled: !!profile,
    staleTime: 30_000,
  });
  const items = useMemo(() => (profile && q.data ? icebergTableItems(profile, q.data.tables, ACTIONS) : []), [profile, q.data]);
  return { items, loading: !!profile && q.isFetching && !q.data, error: q.error ? asError(q.error).message : null, truncated: q.data?.truncated ?? false };
}
