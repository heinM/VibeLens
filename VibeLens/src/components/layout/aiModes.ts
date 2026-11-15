import type { AiMode } from "../../store/useUIStore";

export interface AiModeMeta {
  id: AiMode;
  title: string;
  tagline: string;
  points: string[];
}

/** Copy shared by the first-run chooser and Settings → Interface & AI. */
export const AI_MODES: AiModeMeta[] = [
  {
    id: "classic",
    title: "Classic client",
    tagline: "A fast, focused database client. No AI anywhere.",
    points: ["Editor, results and schema browser", "No agent panel, no AI settings, nothing sent to a model", "Turn AI on later in Settings"],
  },
  {
    id: "assist",
    title: "Assist",
    tagline: "Classic layout with an agent panel you can summon.",
    points: ["Agent docked below the editor; ⌘/Ctrl+J toggles it", "Ask about schema, review SQL, draft migrations", "Every write still needs your approval"],
  },
  {
    id: "agentFirst",
    title: "Agent-first",
    tagline: "Work through the agent; the editor is its canvas.",
    points: ["Agent beside the editor, open by default", "It explores, writes SQL to tabs and asks before acting", "Results and schema stay one glance away"],
  },
];
