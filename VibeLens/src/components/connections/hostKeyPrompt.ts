import { create } from "zustand";
import type { PresentedHostKey } from "../../ipc/types";

interface Pending {
  profileId: string;
  key: PresentedHostKey;
  /** Everyone waiting for the answer to this question. */
  waiters: ((trusted: boolean) => void)[];
}

interface HostKeyPromptState {
  pending: Pending | null;
  /**
   * Shows the key and resolves with the user's decision. Asking again about the same key of the
   * same connection while the dialog is open joins that question; a different question replaces
   * the open one, which resolves as "not trusted".
   */
  ask: (profileId: string, key: PresentedHostKey) => Promise<boolean>;
  answer: (trusted: boolean) => void;
}

const sameQuestion = (p: Pending, profileId: string, key: PresentedHostKey) =>
  p.profileId === profileId && p.key.host === key.host && p.key.port === key.port && p.key.fingerprint === key.fingerprint;

/** The one place the app asks whether to trust an SSH host key while connecting (rendered by `HostKeyPrompt`). */
export const useHostKeyPrompt = create<HostKeyPromptState>((set, get) => ({
  pending: null,
  ask: (profileId, key) =>
    new Promise<boolean>((resolve) => {
      const open = get().pending;
      if (open && sameQuestion(open, profileId, key)) {
        open.waiters.push(resolve);
        return;
      }
      open?.waiters.forEach((w) => w(false));
      set({ pending: { profileId, key, waiters: [resolve] } });
    }),
  answer: (trusted) => {
    const open = get().pending;
    set({ pending: null });
    open?.waiters.forEach((w) => w(trusted));
  },
}));
