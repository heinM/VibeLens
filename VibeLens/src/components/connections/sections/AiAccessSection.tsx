import { Ban, Eye, ShieldCheck, Unlock } from "lucide-react";
import type { AiDataAccess } from "../../../ipc/types";
import { Notice, RadioCards } from "../../ui/formKit";
import { AI_ACCESS } from "../aiAccess";
import type { SectionProps } from "./types";

const ICONS: Record<AiDataAccess, React.ReactNode> = {
  none: <Ban className="size-4" />,
  schemaOnly: <ShieldCheck className="size-4" />,
  resultsWithApproval: <Eye className="size-4" />,
  results: <Unlock className="size-4" />,
};

export function AiAccessSection({ profile, patch }: SectionProps) {
  const access = profile.aiDataAccess;
  return (
    <div className="space-y-4">
      <p className="text-[12px] leading-snug text-muted">
        Choose what the AI agent may send to your model provider for this connection. This is enforced by VibeLens, not by the model.
      </p>
      <RadioCards<AiDataAccess>
        ariaLabel="AI data access"
        value={access}
        onChange={(aiDataAccess) => patch((p) => ({ ...p, aiDataAccess }))}
        options={AI_ACCESS.map((a) => ({
          value: a.value,
          title: a.title,
          icon: ICONS[a.value],
          badge: a.value === "schemaOnly" ? <span className="rounded border border-border px-1 text-[10px] font-medium leading-4 text-muted">Default</span> : undefined,
          description: a.leaves,
          selectedClass: a.value === "results" && profile.environment === "prod" ? "border-danger/70 bg-danger/[0.07]" : a.value === "results" ? "border-warn/60 bg-warn/[0.06]" : undefined,
        }))}
      />
      {access === "results" && profile.environment === "prod" && (
        <Notice tone="danger">
          Production rows will be sent to the provider without asking. Prefer &ldquo;Results, with approval&rdquo;, or point this connection at a local model.
        </Notice>
      )}
      <Notice tone="info">
        Whatever you pick, writes, schema changes and file writes always wait for your approval. SQL classification is advisory, so give the agent&rsquo;s connection a read-only database role.
      </Notice>
    </div>
  );
}
