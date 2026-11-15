// Small building blocks shared by the three Iceberg wizard sections.
import { Plus, X } from "lucide-react";
import type { AwsTarget } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Button, Input } from "../../ui";
import { Switch } from "../../ui/formKit";
import { AwsIdentityPicker } from "../AwsIdentityPicker";
import { defaultAwsTarget } from "../awsHelpers";
import { headerRowError, type HeaderRow } from "../icebergForm";

/** A titled group inside a wizard section (Catalog / Storage / Query engine each have one or two). */
export function Group({ title, description, children }: { title: string; description?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="space-y-3">
      <div>
        <h3 className="text-[13px] font-semibold text-fg">{title}</h3>
        {description && <p className="mt-0.5 text-[12px] leading-snug text-muted">{description}</p>}
      </div>
      {children}
    </section>
  );
}

/**
 * An AWS identity that may borrow the catalog's. With `inherit` set the picker is hidden and `value` is null;
 * turning the switch off reveals the picker seeded from the catalog's region.
 */
export function OptionalAwsIdentity({
  value,
  catalogAws,
  onChange,
  err,
  idPrefix,
  errPrefix,
  inheritLabel = "Use the catalog's AWS identity",
  regionHint,
}: {
  value: AwsTarget | null;
  /** The catalog's identity; null when the catalog has none (then an identity is mandatory). */
  catalogAws: AwsTarget | null;
  onChange: (v: AwsTarget | null) => void;
  err: (field: string) => string | undefined;
  idPrefix: string;
  errPrefix: string;
  inheritLabel?: string;
  regionHint?: string;
}) {
  const scoped = (f: string) => err(errPrefix + f);
  return (
    <div className="space-y-3">
      {catalogAws && (
        <Switch
          checked={value === null}
          onChange={(inherit) => onChange(inherit ? null : defaultAwsTarget(catalogAws.region))}
          label={inheritLabel}
          description={`${catalogAws.region || "no region yet"} · same credentials as the catalog`}
        />
      )}
      {value && <AwsIdentityPicker idPrefix={idPrefix} target={value} err={scoped} onChange={onChange} regionHint={regionHint} />}
    </div>
  );
}

/** Key/value rows for extra REST headers. Rows are controlled by the parent so blanks survive typing. */
export function HeaderEditor({ rows, onChange, error }: { rows: HeaderRow[]; onChange: (rows: HeaderRow[]) => void; error?: string }) {
  const set = (i: number, patch: Partial<HeaderRow>) => onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div className="space-y-1.5">
      {rows.length === 0 && <p className="text-[12px] text-muted">None. Add one for gateways that need an API key or tenant header.</p>}
      {rows.map((r, i) => {
        const rowErr = headerRowError(rows, i);
        return (
          <div key={i} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)_auto] items-center gap-1.5">
            <Input aria-label={`Header ${i + 1} name`} value={r.name} spellCheck={false} autoComplete="off" placeholder="X-Tenant" className={cn("font-mono", rowErr && "!border-danger/70")} onChange={(e) => set(i, { name: e.target.value })} />
            <Input aria-label={`Header ${i + 1} value`} value={r.value} spellCheck={false} autoComplete="off" placeholder="value" className="font-mono" onChange={(e) => set(i, { value: e.target.value })} />
            <button type="button" aria-label={`Remove header ${i + 1}`} onClick={() => onChange(rows.filter((_, j) => j !== i))} className="grid size-7 place-items-center rounded-md text-muted hover:bg-panel-2 hover:text-fg">
              <X className="size-3.5" />
            </button>
          </div>
        );
      })}
      {error && (
        <span role="alert" className="block text-[11px] text-danger">
          {error}
        </span>
      )}
      <Button size="sm" variant="ghost" onClick={() => onChange([...rows, { name: "", value: "" }])}>
        <Plus className="size-3.5" /> Add header
      </Button>
    </div>
  );
}
