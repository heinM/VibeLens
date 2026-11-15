// Which Valkey key the docked inspector is showing (set by the navigator, read by the work area).
import { create } from "zustand";

export interface InspectorTarget {
  profileId: string;
  db: number;
  key: string;
}

interface InspectorState {
  target: InspectorTarget | null;
  open: (t: InspectorTarget) => void;
  close: () => void;
}

export const useInspectorStore = create<InspectorState>((set) => ({
  target: null,
  open: (target) => set({ target }),
  close: () => set({ target: null }),
}));
