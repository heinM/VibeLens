import { create } from "zustand";
import type { PresentedHostKey } from "../../ipc/types";

interface Pending {
  profileId: string;
  key: PresentedHostKey;
  /** Everyone waiting for the answer to this question. */
  waiters: ((trusted: boolean) => void)[];
}

interface HostKeyPromptState {
  /** The question on screen. */
  pending: Pending | null;
  /** Questions about other connections, shown one after another once the open one is answered. */
  queued: Pending[];
  /**
   * Shows the key and resolves with the user's decision. Asking again about the same key of the
   * same connection joins that question. A different key for the same connection replaces its
   * question, which resolves as "not trusted": the server no longer presents the key it was
   * about. A question about another connection waits its turn, so that connecting two profiles
   * at once does not fail the first one unanswered.
   */
  ask: (profileId: string, key: PresentedHostKey) => Promise<boolean>;
  /** Answers the question on screen, and only that one. */
  answer: (trusted: boolean) => void;
}

const sameQuestion = (p: Pending, profileId: string, key: PresentedHostKey) =>
  p.profileId === profileId && p.key.host === key.host && p.key.port === key.port && p.key.fingerprint === key.fingerprint;

/** The one place the app asks whether to trust an SSH host key while connecting (rendered by `HostKeyPrompt`). */
export const useHostKeyPrompt = create<HostKeyPromptState>((set, get) => ({
  pending: null,
  queued: [],
  ask: (profileId, key) =>
    new Promise<boolean>((resolve) => {
      const { pending, queued } = get();
      const all = pending ? [pending, ...queued] : queued;
      const same = all.find((p) => sameQuestion(p, profileId, key));
      if (same) {
        same.waiters.push(resolve);
        return;
      }
      const question: Pending = { profileId, key, waiters: [resolve] };
      const stale = all.find((p) => p.profileId === profileId);
      if (!stale) {
        set(pending ? { queued: [...queued, question] } : { pending: question });
        return;
      }
      // In the place of the question it replaces, so that the other connections keep their turn.
      if (stale === pending) set({ pending: question });
      else set({ queued: queued.map((p) => (p === stale ? question : p)) });
      stale.waiters.forEach((w) => w(false));
    }),
  answer: (trusted) => {
    const { pending, queued } = get();
    set({ pending: queued[0] ?? null, queued: queued.slice(1) });
    pending?.waiters.forEach((w) => w(trusted));
  },
}));
