import { useEffect, useState } from "react";
import { asError } from "../../ipc/commands";
import type { ErrorPayload } from "../../ipc/types";
import { Button, Dialog, ErrorBox, Field, Input } from "../ui";

/** Normalise a user-entered workspace file name: relative, no traversal, `.sql` when no extension. */
export function normaliseFileName(raw: string, defaultExt = "sql"): string | null {
  let name = raw.trim().replace(/\\/g, "/").replace(/^\/+/, "");
  if (!name || name.split("/").some((p) => p === ".." || p === "." || p === "")) return null;
  if (!/\.[A-Za-z0-9]+$/.test(name.split("/").pop() ?? "")) name += `.${defaultExt}`;
  return name;
}

export function SaveAsDialog({
  open,
  initialName,
  onSave,
  onClose,
}: {
  open: boolean;
  initialName: string;
  onSave: (path: string) => Promise<void>;
  onClose: () => void;
}) {
  const [name, setName] = useState(initialName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ErrorPayload | null>(null);

  useEffect(() => {
    if (open) {
      setName(initialName);
      setError(null);
      setBusy(false);
    }
  }, [open, initialName]);

  const submit = async () => {
    const path = normaliseFileName(name);
    if (!path) {
      setError({ code: "invalid_name", message: "Enter a file name without “..” segments, for example reports/monthly.sql." });
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onSave(path);
      onClose();
    } catch (e) {
      setError(asError(e));
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Save worksheet"
      width="max-w-md"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" loading={busy} onClick={() => void submit()}>
            Save
          </Button>
        </>
      }
    >
      <form
        className="space-y-3"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field label="File name" hint="Saved in your VibeLens workspace folder.">
          <Input autoFocus value={name} onChange={(e) => setName(e.target.value)} onFocus={(e) => e.currentTarget.select()} spellCheck={false} autoComplete="off" />
        </Field>
        <ErrorBox error={error} />
      </form>
    </Dialog>
  );
}
