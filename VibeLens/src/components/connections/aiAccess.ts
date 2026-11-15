import type { AiDataAccess } from "../../ipc/types";

export interface AiAccessMeta {
  value: AiDataAccess;
  /** Short label for badges. */
  badge: string;
  title: string;
  /** What leaves the machine, in plain English. */
  leaves: string;
  tone: "muted" | "ok" | "warn" | "danger";
}

export const AI_ACCESS: AiAccessMeta[] = [
  {
    value: "none",
    badge: "No AI access",
    title: "None",
    leaves:
      "The agent cannot look at this connection at all: no schema, no queries. Anything you type into the chat or attach as context (open file, selection) is still sent to the model provider.",
    tone: "muted",
  },
  {
    value: "schemaOnly",
    badge: "Schema only",
    title: "Schema only",
    leaves:
      "Table, column, index and key names, types and comments are sent. Row values never leave your machine. Anything you type or attach in the chat is still sent.",
    tone: "ok",
  },
  {
    value: "resultsWithApproval",
    badge: "Results with approval",
    title: "Results, with approval",
    leaves:
      "Schema as above, plus query results (rows, up to about 24 KB per tool call). You see the query and approve each read before it runs, so you decide which rows are sent.",
    tone: "warn",
  },
  {
    value: "results",
    badge: "Results",
    title: "Results",
    leaves:
      "Schema plus query results (up to about 24 KB per tool call) are sent to the provider automatically, with no prompt for reads. Use this only for non-sensitive data or with a local model.",
    tone: "danger",
  },
];

export const aiAccessMeta = (v: AiDataAccess): AiAccessMeta => AI_ACCESS.find((a) => a.value === v) ?? AI_ACCESS[1];
