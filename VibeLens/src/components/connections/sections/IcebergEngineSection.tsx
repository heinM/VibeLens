import { useId } from "react";
import { Check, Cpu, Cloud, Minus } from "lucide-react";
import type { IcebergQueryEngine } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Input } from "../../ui";
import { FormField, Notice, RadioCards, invalidRing } from "../../ui/formKit";
import { blankEngine, catalogAws, isAwsCatalog } from "../icebergForm";
import { Group, OptionalAwsIdentity } from "./IcebergShared";
import type { SectionProps } from "./types";

type Fact = { ok: boolean | "partial"; text: string };

function Facts({ items }: { items: Fact[] }) {
  return (
    <span className="mt-1.5 block space-y-0.5">
      {items.map((f) => (
        <span key={f.text} className="flex items-start gap-1.5 text-[12px] leading-snug text-muted">
          {f.ok === true ? <Check className="mt-0.5 size-3 shrink-0 text-ok" /> : <Minus className={cn("mt-0.5 size-3 shrink-0", f.ok === "partial" ? "text-warn" : "text-muted/60")} />}
          <span>{f.text}</span>
        </span>
      ))}
    </span>
  );
}

/** Which engine runs SQL: embedded DataFusion on this computer, or Amazon Athena. */
export function IcebergEngineSection({ profile, patchIceberg, err }: SectionProps) {
  const uid = useId();
  const ice = profile.iceberg;
  if (!ice) return null;
  const q = ice.queryEngine;
  const athenaOk = isAwsCatalog(ice.catalog);
  const setEngine = (queryEngine: IcebergQueryEngine) => patchIceberg((o) => ({ ...o, queryEngine }));

  return (
    <div className="space-y-5">
      <Group title="Query engine" description="Who executes your SQL. You can switch at any time; saved queries stay the same.">
        <RadioCards<IcebergQueryEngine["kind"]>
          ariaLabel="Query engine"
          value={q.kind}
          onChange={(k) => setEngine(blankEngine(k, q))}
          options={[
            {
              value: "local",
              title: "Local (DataFusion)",
              icon: <Cpu className="size-4" />,
              badge: <span className="rounded border border-ok/40 bg-ok/10 px-1 text-[10px] font-medium leading-4 text-ok">Free</span>,
              description: (
                <>
                  Runs inside VibeLens. It reads data files directly to this computer, so a large scan also moves that data over your network.
                  <Facts
                    items={[
                      { ok: true, text: "SELECT, time travel and INSERT (append)" },
                      { ok: false, text: "No UPDATE, DELETE or MERGE" },
                      { ok: false, text: "Lake Formation permissions aren't applied" },
                    ]}
                  />
                </>
              ),
            },
            {
              value: "athena",
              title: "Amazon Athena",
              icon: <Cloud className="size-4" />,
              disabled: !athenaOk,
              badge: <span className="rounded border border-warn/40 bg-warn/10 px-1 text-[10px] font-medium leading-4 text-warn">About $5 per TB scanned</span>,
              description: (
                <>
                  Runs in AWS, next to the data. Billed by Athena per query, with a 10 MB minimum. Glue and S3 Tables catalogs only.
                  <Facts
                    items={[
                      { ok: true, text: "Full DML: INSERT, UPDATE, DELETE, MERGE" },
                      { ok: true, text: "Lake Formation aware" },
                      { ok: "partial", text: athenaOk ? "Each query takes a few seconds to start" : "Choose the Glue or S3 Tables catalog to enable" },
                    ]}
                  />
                </>
              ),
            },
          ]}
        />
      </Group>

      {err("ice.engine.catalog") && <Notice tone="danger">{err("ice.engine.catalog")}</Notice>}

      {q.kind === "athena" && (
        <Group title="Athena" description="Queries run in this workgroup. Results are written to the output location, which Athena needs unless the workgroup sets one.">
          <div className="grid gap-3 sm:grid-cols-[minmax(0,14rem)_1fr]">
            <FormField label="Workgroup" htmlFor={`${uid}-wg`} error={err("ice.engine.workgroup")}>
              <Input id={`${uid}-wg`} value={q.workgroup} spellCheck={false} autoComplete="off" placeholder="primary" className={cn("font-mono", err("ice.engine.workgroup") && invalidRing)} onChange={(e) => setEngine({ ...q, workgroup: e.target.value })} />
            </FormField>
            <FormField label="Output location" htmlFor={`${uid}-out`} optional error={err("ice.engine.output")} hint="Empty uses the workgroup's result location.">
              <Input id={`${uid}-out`} value={q.outputLocation ?? ""} spellCheck={false} autoComplete="off" placeholder="s3://my-athena-results/vibelens/" className={cn("font-mono", err("ice.engine.output") && invalidRing)} onChange={(e) => setEngine({ ...q, outputLocation: e.target.value || null })} />
            </FormField>
          </div>
          <OptionalAwsIdentity
            value={q.aws}
            catalogAws={catalogAws(ice.catalog)}
            onChange={(aws) => setEngine({ ...q, aws })}
            err={err}
            idPrefix={`${uid}-aws`}
            errPrefix="ice.engine."
            regionHint="Region of the Athena workgroup."
          />
        </Group>
      )}
    </div>
  );
}
