import { Database, Warehouse, Zap } from "lucide-react";
import type { EngineKind } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { IcebergGlyph } from "../iceberg/IcebergGlyph";

/** The glyph of a connection's engine, in its usual tint. */
export function EngineIcon({ engine, className }: { engine: EngineKind; className?: string }) {
  switch (engine) {
    case "postgres":
      return <Database className={cn("size-4 shrink-0 text-sky-500", className)} />;
    case "iceberg":
      return <IcebergGlyph className={cn("size-4 shrink-0 text-cyan-400", className)} />;
    case "redshift":
      return <Warehouse className={cn("size-4 shrink-0 text-violet-400", className)} />;
    case "valkey":
      return <Zap className={cn("size-4 shrink-0 text-rose-400", className)} />;
  }
}
