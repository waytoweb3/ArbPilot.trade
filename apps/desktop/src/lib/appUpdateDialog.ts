import { create } from "zustand";

import type { PendingUpdate } from "./appUpdate";

interface AppUpdateDialogState {
  pending: PendingUpdate | null;
  open: boolean;
  /** 记录本次运行已自动弹过的版本，避免关闭后再次检查又弹出。 */
  setPending: (pending: PendingUpdate | null) => void;
  show: () => void;
  close: () => void;
}

const autoShown = new Set<string>();

export const useAppUpdateDialog = create<AppUpdateDialogState>((set) => ({
  pending: null,
  open: false,
  setPending: (pending) =>
    set((state) => {
      if (!pending) {
        return { pending: null, open: false };
      }
      const shouldAutoShow = !autoShown.has(pending.info.version);
      if (shouldAutoShow) {
        autoShown.add(pending.info.version);
      }
      return {
        pending,
        open: shouldAutoShow ? true : state.open,
      };
    }),
  show: () => set({ open: true }),
  close: () => set({ open: false }),
}));
