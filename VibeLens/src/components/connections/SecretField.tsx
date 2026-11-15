import { useId } from "react";
import { Lock } from "lucide-react";
import { Button, Input } from "../ui";
import { FormField } from "../ui/formKit";
import type { SecretEdit } from "./draft";

/**
 * Write-only secret input. Never shows a stored value: in `keep` mode it displays a masked
 * placeholder with Replace / Remove, in `set` mode a password input, in `remove` mode an undo.
 */
export function SecretField({
  label,
  edit,
  onChange,
  hasStored,
  optional,
  hint,
  placeholder,
  storedLabel = "•••• stored in keychain",
}: {
  label: string;
  edit: SecretEdit;
  onChange: (e: SecretEdit) => void;
  /** A secret is (presumably) already saved for this connection. */
  hasStored: boolean;
  optional?: boolean;
  hint?: string;
  placeholder?: string;
  storedLabel?: string;
}) {
  const id = useId();
  if (hasStored && edit.mode === "keep") {
    return (
      <FormField label={label} htmlFor={id} optional={optional}>
        <div className="flex items-center gap-2">
          <div id={id} className="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md border border-border bg-panel-2/50 px-2.5 text-[13px] text-muted">
            <Lock className="size-3.5 shrink-0" />
            <span className="truncate font-mono">{storedLabel}</span>
          </div>
          <Button size="sm" onClick={() => onChange({ mode: "set", value: "" })}>
            Replace
          </Button>
          <Button size="sm" variant="ghost" onClick={() => onChange({ mode: "remove", value: "" })}>
            Remove
          </Button>
        </div>
      </FormField>
    );
  }
  if (edit.mode === "remove") {
    return (
      <FormField label={label} htmlFor={id} optional={optional}>
        <div className="flex items-center gap-2">
          <div id={id} className="flex h-8 min-w-0 flex-1 items-center rounded-md border border-warn/40 bg-warn/10 px-2.5 text-[12px] text-warn">
            Will be removed from the keychain when you save.
          </div>
          <Button size="sm" variant="ghost" onClick={() => onChange({ mode: "keep", value: "" })}>
            Undo
          </Button>
        </div>
      </FormField>
    );
  }
  return (
    <FormField label={label} htmlFor={id} optional={optional} hint={hint ?? "Saved to the OS keychain. It is never written to disk or shown again."}>
      <div className="flex items-center gap-2">
        <Input
          id={id}
          type="password"
          autoComplete="new-password"
          spellCheck={false}
          value={edit.value}
          placeholder={placeholder}
          onChange={(e) => onChange({ mode: "set", value: e.target.value })}
        />
        {hasStored && (
          <Button size="sm" variant="ghost" onClick={() => onChange({ mode: "keep", value: "" })}>
            Keep existing
          </Button>
        )}
      </div>
    </FormField>
  );
}
